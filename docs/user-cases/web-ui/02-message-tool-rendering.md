# Message & Tool Rendering — Web UI Acceptance Test Cases

Scope: end-user-observable behavior of how the chat shell renders user messages, assistant messages (text, thinking, tool-call cards in content order, usage, error and aborted states), tool-call cards and their per-tool renderers (bash, calculate, get-current-time, default JSON), the raw tool-debug inspector, the aborted stub, the thinking block, and the shared UI primitives (collapsible headers, code blocks, console blocks). All assertions are phrased against what a person sees and does in the browser, not against source internals.

---

### WUI-MSG-01: User text message renders markdown content in its bubble

- **Persona:** A developer chatting with the agent who has just typed a plain prompt.
- **Preconditions:** A live session is connected; the chat transcript is visible; no attachments are involved.
- **Steps:**
  1. Type `List the files in this folder` into the editor and send it.
  2. Observe the message that appears in the transcript.
- **Assertions:**
  - A1: The sent text appears in a rounded bubble aligned to the left edge of the chat column (not centered, not full-width).
  - A2: The exact characters typed are shown, with original line breaks and spacing preserved (whitespace is not collapsed onto one line).
  - A3: No attachment chips, delete buttons, or thumbnails appear under the text for a plain message.
  - A4: A long word or URL with no spaces wraps inside the bubble rather than forcing the bubble or page to scroll horizontally.
- **Traces:** matrix row 35; M2.

---

### WUI-MSG-02: User message with attachments shows read-only attachment chips

- **Persona:** A developer who dragged a PDF and an image into the editor before sending.
- **Preconditions:** Two attachments (one image, one PDF) are staged on the message; the message has been sent.
- **Steps:**
  1. Send the message with the two attachments and locate it in the transcript.
  2. Inspect the row beneath the message text.
- **Assertions:**
  - A1: A row of attachment tiles appears directly below the message text, one tile per attachment, in the order attached.
  - A2: The tiles wrap onto additional lines when there are more than fit on one row, with consistent spacing between them.
  - A3: The tiles are read-only in the transcript: there is no delete/remove control on a tile in a sent message.
  - A4: Clicking a tile opens the attachment overlay/preview rather than deleting or editing it.
  - A5: If the message text is empty but attachments exist, the tiles still render.
- **Traces:** matrix row 35; M2.

---

### WUI-MSG-03: Assistant message renders text, thinking, and tool cards in original content order

- **Persona:** A developer reading a completed assistant turn that mixed reasoning, prose, and a tool call.
- **Preconditions:** A finished assistant turn whose content was, in order: a thinking block, a paragraph of text, a tool call, then another paragraph.
- **Steps:**
  1. Scroll to the completed assistant turn.
  2. Read the rendered pieces top to bottom.
- **Assertions:**
  - A1: The rendered pieces appear in exactly the same order the assistant produced them: thinking block, first paragraph, tool card, second paragraph.
  - A2: Each text paragraph renders as its own block with preserved line breaks.
  - A3: The tool call renders as a card/row interleaved between the paragraphs, not relocated to the top or bottom of the message.
  - A4: There is consistent vertical spacing between the ordered pieces.
- **Traces:** matrix rows 36, 37; M2.

---

### WUI-MSG-04: Empty/whitespace-only text and thinking chunks are not rendered

- **Persona:** A developer whose model emitted an empty text fragment and an empty reasoning fragment.
- **Preconditions:** An assistant message that contains a text chunk that is only whitespace and a thinking chunk that is only whitespace, plus one real paragraph.
- **Steps:**
  1. View the assistant message.
- **Assertions:**
  - A1: No empty text block is shown for the whitespace-only text chunk (no blank gap where a paragraph would be).
  - A2: No "Thinking..." header is shown for the whitespace-only thinking chunk.
  - A3: The one real paragraph renders normally.
- **Traces:** matrix row 36; M2.

---

### WUI-MSG-05: Usage/cost summary appears only after streaming finishes

- **Persona:** A developer watching a turn complete and then checking token cost.
- **Preconditions:** A model that reports usage; a turn that is actively streaming, then completes.
- **Steps:**
  1. Send a prompt and watch the assistant reply stream in.
  2. Note whether a usage line is present while tokens are still arriving.
  3. Wait for the turn to finish and look again.
- **Assertions:**
  - A1: While the message is still streaming, no usage/cost line is shown beneath it.
  - A2: After streaming completes, a small muted usage line appears beneath the message text.
  - A3: The usage line shows the reported figures (e.g., input/output token counts and a total cost) and omits any zero-valued figures rather than showing `0`.
  - A4: If the completed message has no usage data, no usage line appears at all.
- **Traces:** matrix row 36; M2.

---

### WUI-MSG-06: Clicking the usage line opens the cost detail (when wired)

- **Persona:** A developer who wants the per-turn cost breakdown.
- **Preconditions:** A completed assistant message with usage; the shell is configured so the usage line is clickable (a cost-click handler is wired).
- **Steps:**
  1. Hover over the usage line beneath a completed message.
  2. Click it.
- **Assertions:**
  - A1: On hover the usage line changes color/affordance (cursor indicates it is clickable), distinguishing it from the non-clickable variant.
  - A2: Clicking it triggers the cost detail action (e.g., opens the session stats view).
  - A3: When no cost-click handler is wired, the usage line is shown as plain muted text with no pointer cursor and clicking does nothing.
- **Traces:** matrix row 36; M2.

---

### WUI-MSG-07: Assistant error turn shows a distinct error box with the message

- **Persona:** A developer whose request failed (e.g., provider/auth error).
- **Preconditions:** A turn that ended in an error state carrying an error message string.
- **Steps:**
  1. Trigger a turn that fails and view the resulting assistant message.
- **Assertions:**
  - A1: A bordered box in a destructive/red tint appears beneath any partial content.
  - A2: The box is labeled "Error:" (translated to the active language) followed by the provider's error text.
  - A3: The error text is contained within the box and does not overflow horizontally off the page.
  - A4: Any partial assistant text produced before the failure is still shown above the error box.
- **Traces:** matrix row 36; M2.

---

### WUI-MSG-08: Aborted assistant turn shows the italic "Request aborted" stub

- **Persona:** A developer who pressed Stop/Escape mid-stream.
- **Preconditions:** A turn was cancelled while streaming, before completion.
- **Steps:**
  1. Send a prompt, then abort the turn before it finishes.
  2. View the resulting assistant message.
- **Assertions:**
  - A1: An italic, destructive-colored "Request aborted" notice (translated) appears at the end of the message.
  - A2: Any partial text/thinking/tool content produced before the abort remains visible above the stub.
  - A3: No usage line is shown for the aborted turn.
- **Traces:** matrix rows 36, 39; M2.

---

### WUI-MSG-09: Standalone aborted-message stub renders the cancellation notice

- **Persona:** A developer reviewing history that includes a recorded cancellation entry.
- **Preconditions:** The transcript contains a recorded aborted entry rendered on its own.
- **Steps:**
  1. Scroll to the aborted history entry.
- **Assertions:**
  - A1: It shows the italic, destructive-colored "Request aborted" text (translated).
  - A2: The notice uses the small text size consistent with other status notices.
- **Traces:** matrix row 39; M2.

---

### WUI-MSG-10: Thinking block is collapsed by default and shows a "Thinking..." header

- **Persona:** A developer reading a completed turn that contains reasoning.
- **Preconditions:** A finished assistant message that includes a non-empty thinking chunk.
- **Steps:**
  1. View the assistant message.
- **Assertions:**
  - A1: A "Thinking..." header row with a right-pointing chevron is shown.
  - A2: The reasoning body is hidden by default; only the header is visible.
  - A3: On a finished (non-streaming) turn the "Thinking..." text is static (no animated shimmer).
- **Traces:** matrix row 40; M2.

---

### WUI-MSG-11: Thinking header shimmers while reasoning is streaming

- **Persona:** A developer watching the model think in real time.
- **Preconditions:** A turn is actively streaming and currently emitting thinking content.
- **Steps:**
  1. Send a prompt to a reasoning-capable model and watch the in-flight thinking block.
- **Assertions:**
  - A1: While streaming, the "Thinking..." header text shows an animated shimmer/gradient sweep (it pulses), not static text.
  - A2: Once the turn finishes streaming, the shimmer stops and the header becomes static.
- **Traces:** matrix row 40; M2.

---

### WUI-MSG-12: Expanding and collapsing a thinking block toggles the reasoning body and chevron

- **Persona:** A developer who wants to read the model's full reasoning.
- **Preconditions:** A finished assistant message with a non-empty thinking block (collapsed).
- **Steps:**
  1. Click the "Thinking..." header.
  2. Read the revealed reasoning.
  3. Click the header again.
- **Assertions:**
  - A1: After the first click, the reasoning body is revealed below the header, rendered as text with preserved formatting.
  - A2: The chevron rotates to point down (expanded state) on expand and back to the right (collapsed) on the second click.
  - A3: After the second click, the reasoning body is hidden again.
  - A4: The header text and chevron are not selectable as text (clicking does not start a text selection).
- **Traces:** matrix row 40; M2.

---

### WUI-MSG-13: A pending (in-flight) tool call shows a spinner header

- **Persona:** A developer watching a tool execute.
- **Preconditions:** A tool call has started but no result has arrived yet (the turn is streaming).
- **Steps:**
  1. Trigger a turn that calls a tool with a noticeable runtime.
  2. Observe the tool card while it is running.
- **Assertions:**
  - A1: The tool card shows a status header with the tool's icon and an animated spinner on the right while it is in progress.
  - A2: The header text reflects the in-progress phase (e.g., the tool's "running/waiting" wording).
  - A3: When the result arrives, the spinner is replaced by a completed (or error) state.
- **Traces:** matrix rows 37, 191; M2.

---

### WUI-MSG-14: Completed vs. error tool state is color-coded in the header

- **Persona:** A developer scanning a turn for which tool calls succeeded.
- **Preconditions:** One assistant turn containing a tool call that succeeded and another tool call that returned an error result.
- **Steps:**
  1. View the two finished tool cards.
- **Assertions:**
  - A1: The successful tool's header icon is shown in green.
  - A2: The errored tool's header icon is shown in the destructive/red color.
  - A3: Neither finished card shows the spinner.
- **Traces:** matrix rows 37, 191; M2.

---

### WUI-MSG-15: Aborted tool call (no result) is shown in the error state

- **Persona:** A developer who aborted a turn while a tool was still running.
- **Preconditions:** A turn aborted with a tool call that never received a result.
- **Steps:**
  1. Abort a turn while a tool is mid-execution.
  2. View the tool card for that call.
- **Assertions:**
  - A1: The tool card is rendered in its error/destructive state (the icon is red), not stuck on the spinner.
  - A2: No success output is shown for that call (the synthesized aborted result carries no content).
  - A3: The card is no longer marked as "in progress" after the abort.
- **Traces:** matrix row 37; M2.

---

### WUI-MSG-16: Custom-chrome tool renderers are not wrapped in the default card

- **Persona:** A developer comparing how different tools render.
- **Preconditions:** A turn containing a tool whose renderer owns its own chrome (custom) and a tool whose renderer relies on the default card wrapper.
- **Steps:**
  1. View both tool outputs.
- **Assertions:**
  - A1: The default-style tool output is enclosed in a bordered, rounded card with the card background.
  - A2: The custom-chrome tool output is rendered without that extra surrounding card (it provides its own visual container).
  - A3: Neither tool is double-wrapped (no card-inside-card).
- **Traces:** matrix row 37; M2.

---

### WUI-MSG-17: Tool debug inspector shows raw call args and result as code blocks

- **Persona:** A power user inspecting the exact JSON sent to and returned from a tool.
- **Preconditions:** The raw tool-debug view is shown for a completed tool call whose arguments and result are JSON.
- **Steps:**
  1. Open the debug/raw view for the tool call.
- **Assertions:**
  - A1: A "Call" section shows the call arguments pretty-printed as JSON in a code block labeled `json`.
  - A2: A "Result" section shows the result text as a code block; when the text is valid JSON it is pretty-printed and labeled `json`, otherwise it is shown verbatim labeled `text`.
  - A3: When there is no result yet, the Result section reads "(no result)" instead of an empty code block.
  - A4: Both the section labels ("Call", "Result") respect the active language.
- **Traces:** matrix row 38; M2.

---

### WUI-MSG-18: Bash tool — waiting state before any command arrives

- **Persona:** A developer watching a bash tool call begin to stream.
- **Preconditions:** A bash tool call has started but its `command` argument has not streamed in yet.
- **Steps:**
  1. Trigger a bash tool call and observe the card at the very first moment.
- **Assertions:**
  - A1: The card shows a terminal icon header with "Waiting for command..." (translated).
  - A2: No console/output block is shown yet (there is nothing to display).
- **Traces:** matrix row 48; M2.

---

### WUI-MSG-19: Bash tool — command echoed before output arrives

- **Persona:** A developer watching the command stream in before it has run.
- **Preconditions:** A bash tool call whose `command` argument has arrived but whose result has not.
- **Steps:**
  1. Observe the bash card after the command appears but before output.
- **Assertions:**
  - A1: The header shows the terminal icon with the "Running command..." (translated) text and a spinner.
  - A2: A console block appears showing the command prefixed with `> ` (e.g., `> ls -la`).
  - A3: No command output is shown yet (only the echoed command line).
- **Traces:** matrix rows 48, 191; M2.

---

### WUI-MSG-20: Bash tool — successful command shows command then output

- **Persona:** A developer reading the result of a finished shell command.
- **Preconditions:** A bash tool call that finished successfully (non-error result) with captured stdout.
- **Steps:**
  1. View the completed bash card.
- **Assertions:**
  - A1: The header icon is green (complete state).
  - A2: A console block shows the command line (`> <command>`) followed by a blank line and then the captured output.
  - A3: The console block uses the default (non-error) text color.
  - A4: If the command produced no output, only the `> <command>` line is shown (no spurious blank output area).
- **Traces:** matrix rows 48, 191; M2.

---

### WUI-MSG-21: Bash tool — failed command uses the error console variant

- **Persona:** A developer whose shell command exited non-zero.
- **Preconditions:** A bash tool call whose result is an error (non-zero exit / stderr).
- **Steps:**
  1. View the completed bash card for the failed command.
- **Assertions:**
  - A1: The header icon is shown in the destructive/red color (error state).
  - A2: The console block renders in the error variant: its text is shown in the destructive color rather than the default foreground.
  - A3: The command line (`> <command>`) and the error output are both visible in the console block.
- **Traces:** matrix rows 48, 191; M2.

---

### WUI-MSG-22: Calculate tool — four progressive states render correctly

- **Persona:** A developer watching the calculate tool fill in over a streaming turn.
- **Preconditions:** A calculate tool call observed across its lifecycle, plus a separately completed successful calculation.
- **Steps:**
  1. Observe the card when no params have arrived.
  2. Observe it when params exist but the expression is still empty.
  3. Observe it when the full expression has arrived but no result yet.
  4. Observe a separate completed successful calculation (e.g., `2 + 2` → `4`).
- **Assertions:**
  - A1: With no params: the header reads "Waiting for expression..." (translated) with the calculator icon.
  - A2: With params but empty expression: the header reads "Writing expression..." (translated).
  - A3: With a full expression and no result: the header reads "Calculating <expression>" (translated label + the expression).
  - A4: On success: the header reads `<expression> = <result>` (e.g., `2 + 2 = 4`) with a green icon, and no separate output block is shown.
- **Traces:** matrix row 49; M2.

---

### WUI-MSG-23: Calculate tool — error result shows expression in header and message below

- **Persona:** A developer who gave the calculator an invalid expression.
- **Preconditions:** A calculate tool call that returned an error result for a given expression.
- **Steps:**
  1. View the completed calculate card for the failed expression.
- **Assertions:**
  - A1: The header shows the calculator icon (in the error/red color) followed by the original expression.
  - A2: The error message text is shown on its own line below the header, in the destructive color.
  - A3: The header does NOT show `expression = result` (no fabricated success line).
- **Traces:** matrix row 49; M2.

---

### WUI-MSG-24: get_current_time tool — timezone vs. no-timezone phrasing

- **Persona:** A developer calling the current-time tool with and without a timezone.
- **Preconditions:** Two completed get_current_time calls: one with a `timezone` argument and one without, both successful.
- **Steps:**
  1. View the completed card for the call that specified a timezone.
  2. View the completed card for the call with no timezone.
- **Assertions:**
  - A1: The timezone call's header reads "Getting current time in <timezone>: <result>" (translated label).
  - A2: The no-timezone call's header reads "Getting current date and time: <result>" (translated label).
  - A3: Both completed headers show the clock icon in green and put the returned time inline in the header (no separate output block).
- **Traces:** matrix row 51; M2.

---

### WUI-MSG-25: get_current_time tool — in-flight and error paths

- **Persona:** A developer observing the time tool while it runs and when it fails.
- **Preconditions:** A get_current_time call observed before any params arrive, and a separate call that returned an error.
- **Steps:**
  1. Observe the card before any params/result (in-flight).
  2. View a separate completed card whose result is an error.
- **Assertions:**
  - A1: Before any params/result, the header reads "Getting time..." (translated) with the clock icon.
  - A2: For the error result, the header shows the clock icon (error color) with the descriptive text and the error message rendered on its own line below in the destructive color.
  - A3: The error card does not show a successful time value in the header.
- **Traces:** matrix row 51; M2.

---

### WUI-MSG-26: Default renderer pretty-prints Input and Output JSON for unknown tools

- **Persona:** A developer using a tool that has no custom renderer.
- **Preconditions:** A completed call to a tool with no registered renderer, whose arguments and result are JSON.
- **Steps:**
  1. View the completed tool card.
- **Assertions:**
  - A1: The card header reads "Tool Call" (translated) with a generic code icon, colored green for the completed state.
  - A2: An "Input" section shows the call arguments pretty-printed (indented) as a `json` code block.
  - A3: An "Output" section shows the result; valid-JSON output is pretty-printed and labeled `json`, otherwise it is shown as `text`.
  - A4: When the tool produced no output, the Output block reads "(no output)" (translated) rather than being empty.
- **Traces:** matrix row 50; M2.

---

### WUI-MSG-27: Default renderer — streaming/preparing states before params resolve

- **Persona:** A developer watching an unknown tool start before its arguments are complete.
- **Preconditions:** A default-rendered tool call observed while streaming, before/while arguments arrive.
- **Steps:**
  1. Observe the card when no params have arrived yet (still streaming).
  2. Observe it when params are present but not yet a complete/non-empty object (still streaming).
- **Assertions:**
  - A1: With no params and still streaming, the header reads "Preparing tool..." (translated) with a spinner.
  - A2: With params that are still empty/incomplete while streaming, the header reads "Preparing tool parameters..." (translated).
  - A3: Once usable params exist (and no result yet), the card switches to the "Tool Call" header with an Input code block.
- **Traces:** matrix row 50; M2.

---

### WUI-MSG-28: setShowJsonMode(true) forces every tool through the default JSON renderer

- **Persona:** A power user who turned on raw/JSON tool view to debug.
- **Preconditions:** A turn that includes a bash call and a calculate call; JSON/raw mode is then enabled.
- **Steps:**
  1. View the bash and calculate cards with JSON mode OFF (rich renderers).
  2. Enable JSON mode (setShowJsonMode true) and view the same kinds of tool calls.
- **Assertions:**
  - A1: With JSON mode OFF, bash shows its console block and calculate shows its `expression = result` header (the rich, tool-specific layouts).
  - A2: With JSON mode ON, both tools instead render through the default renderer: a "Tool Call" header with Input/Output JSON code blocks.
  - A3: No tool-specific layout (console block, `expression = result`) is shown while JSON mode is ON, even for tools that have a custom renderer.
  - A4: Turning JSON mode back OFF restores the rich, tool-specific rendering.
- **Traces:** matrix row 47; M2.

---

### WUI-MSG-29: Collapsible tool header expands/collapses content with a chevron swap

- **Persona:** A developer expanding a tool card that uses a collapsible header.
- **Preconditions:** A tool card whose body is behind a collapsible header (collapsed by default).
- **Steps:**
  1. Locate the collapsible header (it shows a double-chevron "expand" glyph when collapsed).
  2. Click the header to expand.
  3. Click it again to collapse.
- **Assertions:**
  - A1: When collapsed, the body content is hidden (zero height) and the header shows the collapsed (double-chevron) glyph.
  - A2: Clicking to expand smoothly grows the content area open (animated max-height transition) and reveals the body.
  - A3: On expand the glyph swaps to the single up-chevron (collapse) icon; on collapse it swaps back to the double-chevron.
  - A4: The whole header row is clickable (full width) and shows a hover affordance.
  - A5: Clicking the header does not submit a form or scroll the page to the top (default action is suppressed).
- **Traces:** matrix row 46; M2.

---

### WUI-MSG-30: Code block copy button copies content and shows confirmation

- **Persona:** A developer copying a JSON payload from a tool's code block.
- **Preconditions:** A code block (e.g., a default-renderer Input/Output block) is visible with content.
- **Steps:**
  1. Note the language label shown in the code block header (e.g., `json`).
  2. Click the copy button in the code block header.
  3. Paste into a scratch buffer to confirm.
- **Assertions:**
  - A1: The code block header shows the language label on the left (e.g., `json`).
  - A2: Clicking copy puts the exact code text on the clipboard (the pasted content matches what is shown).
  - A3: The copy button briefly changes to a check icon with a "Copied!" (translated) confirmation, then reverts after a short delay.
  - A4: The code is shown in a monospace block; long lines scroll horizontally within the block without breaking page layout.
- **Traces:** matrix rows 38, 50; M2.

---

### WUI-MSG-31: Console block scrolls, auto-pins to bottom, and copies output

- **Persona:** A developer viewing a long bash output.
- **Preconditions:** A bash (or other console-block) result whose output exceeds the visible height of the console pane.
- **Steps:**
  1. View the console block for the long output.
  2. Observe the scroll position as new output streams in.
  3. Click the copy button in the console header.
- **Assertions:**
  - A1: The console pane is height-capped and scrolls internally; long output does not push the page layout.
  - A2: As content updates/streams, the pane auto-scrolls to keep the latest output (the bottom) in view.
  - A3: The header shows a "console" label (translated) and a copy button.
  - A4: Clicking copy places the full console content on the clipboard and briefly shows the "Copied!" confirmation.
  - A5: Output text wraps within the pane (long lines are not truncated off-screen).
- **Traces:** matrix rows 42, 48; M2.

---

### WUI-MSG-32: Console block error variant is visually distinct from default

- **Persona:** A developer distinguishing failed command output at a glance.
- **Preconditions:** One default-variant console block and one error-variant console block visible.
- **Steps:**
  1. Compare the two console blocks side by side.
- **Assertions:**
  - A1: The default console block renders its text in the normal foreground color.
  - A2: The error console block renders its text in the destructive/red color.
  - A3: Both still expose the same "console" label and working copy button.
- **Traces:** matrix row 42; M2.

---

### WUI-MSG-33: Expandable section reveals captured children only when opened

- **Persona:** A developer expanding a collapsible details section (e.g., reasoning/details accordion).
- **Preconditions:** An expandable section with a summary label and hidden detail children, collapsed by default.
- **Steps:**
  1. View the section collapsed.
  2. Click the summary row to expand.
  3. Click again to collapse.
- **Assertions:**
  - A1: When collapsed, only the summary label and a right-pointing chevron are visible; the detail content is not shown.
  - A2: Clicking the summary reveals the original detail content below it intact (the captured children render correctly, not lost or duplicated).
  - A3: The chevron changes from right-pointing (collapsed) to down-pointing (expanded) and back.
  - A4: A section configured to default-expanded shows its content immediately on first view without a click.
- **Traces:** matrix row 41; M2.

---

### WUI-MSG-34: A turn with no renderable content produces no empty bubble

- **Persona:** A developer whose assistant turn ended with only suppressed/empty content (e.g., all chunks empty and tool calls hidden).
- **Preconditions:** An assistant message whose only chunks are empty text/thinking, with no usage, no error, and not aborted.
- **Steps:**
  1. View where that message would appear in the transcript.
- **Assertions:**
  - A1: No empty content container/padding is rendered for the message (no stray blank block).
  - A2: The transcript flows directly to the next entry with no visible gap attributable to the empty message.
- **Traces:** matrix row 36; M2.

---

### WUI-MSG-35: A standalone tool-result entry is not rendered on its own line

- **Persona:** A developer scrolling history that contains tool-result records.
- **Preconditions:** The transcript history includes tool-result records that are paired into their assistant messages by call id.
- **Steps:**
  1. Scroll through the history.
- **Assertions:**
  - A1: Tool results appear only inside their owning assistant message's tool card (paired by call id), interleaved in content order.
  - A2: No duplicate, standalone tool-result row appears separately in the history list for the same call.
- **Traces:** matrix rows 37, 44; M2.
