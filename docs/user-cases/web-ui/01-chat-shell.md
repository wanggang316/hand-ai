# Chat Shell & Conversation Flow — Web UI Acceptance Test Cases

Scope: end-user behavior of the web UI chat shell — the split chat/artifacts layout, auto-scroll, live streaming, abort affordances, per-turn cost stats, the message editor, and the stable message list across multi-turn conversations.

### WUI-CHAT-01: Side-by-side split appears once an artifact exists on a wide window
- **Persona:** Knowledge worker on a desktop browser
- **Preconditions:** Browser window is at least 800px wide; a conversation is open with no artifacts yet; the chat occupies the full width
- **Steps:**
  1. Send a prompt that makes the assistant create one artifact.
  2. Wait for the artifact to be created.
- **Assertions:**
  - A1: The chat column shrinks to occupy the left half of the window (roughly 50% width).
  - A2: The artifacts panel becomes visible on the right half of the window (roughly 50% width).
  - A3: The two panels sit side by side; neither overlaps the other.
  - A4: No floating "Artifacts" pill is shown while the panel is open.
- **Traces:** rows 12, 13; M1

### WUI-CHAT-02: Mobile/narrow window shows artifacts as a full-screen overlay
- **Persona:** User on a phone-width browser
- **Preconditions:** Browser window is narrower than 800px; a conversation has produced at least one artifact and the artifacts panel is open
- **Steps:**
  1. Observe the layout while the artifacts panel is open.
- **Assertions:**
  - A1: The artifacts panel covers the full window (it is an overlay, not a side column).
  - A2: The chat column is fully obscured by the overlay while it is shown.
  - A3: The chat column remains full-width underneath (it does not shrink to 50%).
- **Traces:** row 12; M1

### WUI-CHAT-03: Floating "Artifacts N" pill appears when artifacts exist but panel is collapsed
- **Persona:** User who closed the artifacts panel to focus on chat
- **Preconditions:** A conversation has produced two artifacts; the artifacts panel is currently closed/collapsed
- **Steps:**
  1. Observe the top-center area of the chat column.
- **Assertions:**
  - A1: A floating pill labelled "Artifacts" is shown, horizontally centered near the top of the panel.
  - A2: The pill displays the current artifact count "2".
  - A3: The pill is absent whenever no artifacts exist.
- **Traces:** row 13; M1

### WUI-CHAT-04: Clicking the floating pill reopens the artifacts panel
- **Persona:** User who wants to revisit a generated artifact
- **Preconditions:** Artifacts exist; the panel is collapsed; the floating "Artifacts N" pill is visible
- **Steps:**
  1. Click the floating "Artifacts" pill.
- **Assertions:**
  - A1: The artifacts panel opens (side-by-side on a wide window, full-screen overlay on a narrow window).
  - A2: The floating pill disappears once the panel is open.
- **Traces:** rows 12, 13; M1

### WUI-CHAT-05: A newly created artifact auto-opens the panel; restored history does not
- **Persona:** User starting a fresh session vs. reopening a saved one
- **Preconditions:** Two distinct situations are tested: (a) a live conversation with the panel closed; (b) a previously saved session with artifacts that is reloaded fresh
- **Steps:**
  1. In situation (a), send a prompt that creates a brand-new artifact.
  2. In situation (b), reload/restore the saved session.
- **Assertions:**
  - A1: In (a), the artifacts panel opens automatically when the net-new artifact is created.
  - A2: In (b), the panel stays closed after restore even though artifacts exist.
  - A3: In (b), the floating "Artifacts N" pill is shown with the correct restored count.
- **Traces:** rows 13, 15; M1

### WUI-CHAT-06: Empty conversation shows the editor with no message bubbles
- **Persona:** First-time user opening the app
- **Preconditions:** A fresh session with no messages
- **Steps:**
  1. Observe the chat shell before typing anything.
- **Assertions:**
  - A1: The message editor is visible, anchored at the bottom of the chat column.
  - A2: No message bubbles, tool cards, or streaming cursor are shown in the history area.
  - A3: The cost stats bar shows no usage figures (it is effectively empty).
- **Traces:** rows 16, 20; M1

### WUI-CHAT-07: Pulsing cursor shows before the first streamed token
- **Persona:** User who just submitted a prompt
- **Preconditions:** A model is selected; the editor has text
- **Steps:**
  1. Press Enter to send the prompt.
  2. Observe the history area before the assistant emits any visible text.
- **Assertions:**
  - A1: A small pulsing block (animated cursor) appears in the streaming area immediately after sending, before any assistant text is shown.
  - A2: The send button has switched to a stop button while the agent is working.
- **Traces:** rows 21, 29, 33; M1

### WUI-CHAT-08: Tokens render live as the assistant streams
- **Persona:** User watching a response generate
- **Preconditions:** A prompt has been sent and the assistant has begun streaming text
- **Steps:**
  1. Observe the streaming message as the response arrives.
- **Assertions:**
  - A1: Assistant text grows incrementally in the streaming area as new tokens arrive.
  - A2: A pulsing cursor remains visible at the end of the streamed text while streaming continues.
- **Traces:** rows 27, 28, 29; M1

### WUI-CHAT-09: Clean hand-off from streaming container to the stable list on completion
- **Persona:** User reading a finished reply
- **Preconditions:** The assistant has just finished streaming a complete message
- **Steps:**
  1. Wait for the response to complete (agent stops working).
- **Assertions:**
  - A1: The completed assistant message appears in the stable history list.
  - A2: The pulsing cursor is gone once the message is finalized.
  - A3: The message is not duplicated — it appears exactly once, not in both the streaming area and the stable list.
  - A4: The stop button reverts to the send button.
- **Traces:** rows 24, 27, 29, 33; M1

### WUI-CHAT-10: In-flight tool card is never rendered twice
- **Persona:** User watching the assistant invoke a tool
- **Preconditions:** A prompt triggers the assistant to call a tool that is still executing
- **Steps:**
  1. Observe the chat while a tool call is in progress (pending).
- **Assertions:**
  - A1: The in-progress tool card is shown exactly once (rendered by the streaming area).
  - A2: The same pending tool call does not also appear as a separate card in the stable history list.
  - A3: After the tool finishes and the turn ends, the tool result card appears once in the stable history.
- **Traces:** rows 10, 26, 27; M1

### WUI-CHAT-11: Auto-scroll keeps the view pinned to the latest content
- **Persona:** User following a long streaming reply
- **Preconditions:** A conversation tall enough to scroll; the user has not manually scrolled up
- **Steps:**
  1. Send a prompt that produces a long response.
  2. Watch the view as content streams in.
- **Assertions:**
  - A1: The view stays pinned to the bottom, revealing newly added lines as they stream.
  - A2: The latest content remains visible without any manual scrolling.
- **Traces:** rows 17, 19; M1

### WUI-CHAT-12: Scrolling up disables auto-scroll
- **Persona:** User reviewing earlier parts of a streaming reply
- **Preconditions:** A response is streaming and the view is currently pinned to the bottom
- **Steps:**
  1. While content is still streaming, scroll upward by more than a small margin (well away from the bottom).
- **Assertions:**
  - A1: The view stays at the user's scroll position and does not jump back to the bottom as new content arrives.
  - A2: New streamed content is appended below without forcing the viewport to move.
- **Traces:** row 17; M1

### WUI-CHAT-13: Scrolling back near the bottom re-enables auto-scroll
- **Persona:** User who scrolled up and now wants to follow along again
- **Preconditions:** Auto-scroll was disabled by a prior scroll-up; a response is still streaming
- **Steps:**
  1. Scroll back down until the viewport is within a few pixels of the bottom.
  2. Continue watching as more content streams.
- **Assertions:**
  - A1: Once the view is near the bottom, it re-pins and resumes following new content automatically.
- **Traces:** row 17; M1

### WUI-CHAT-14: Stats bar appearing does not false-disable auto-scroll
- **Persona:** User watching the first reply of a session complete
- **Preconditions:** The view is pinned to the bottom; the cost stats bar is not yet populated (first turn, no totals)
- **Steps:**
  1. Let the first assistant turn complete so usage totals populate and the stats bar appears at the bottom.
- **Assertions:**
  - A1: Auto-scroll remains enabled; the view stays pinned to the bottom after the stats bar appears.
  - A2: The shrink of the scroll area caused by the stats bar does not cause the view to detach from the bottom.
- **Traces:** row 18; M1

### WUI-CHAT-15: Sending re-arms auto-scroll for the new turn
- **Persona:** User who had scrolled up, then sends a new message
- **Preconditions:** Auto-scroll was disabled because the user scrolled up earlier
- **Steps:**
  1. Type a new message and send it.
- **Assertions:**
  - A1: On send, the view re-pins to the bottom so the new turn is visible.
  - A2: The newly sent user message is visible at the bottom of the history.
- **Traces:** rows 17, 34; M1

### WUI-CHAT-16: Stop button aborts an in-progress turn
- **Persona:** User who wants to cancel a long-running reply
- **Preconditions:** The assistant is actively streaming; the editor's right control shows a stop button
- **Steps:**
  1. Click the stop button.
- **Assertions:**
  - A1: The agent stops working; the stop button reverts to the send button.
  - A2: The pulsing cursor disappears and no further tokens are appended.
  - A3: The partial response that was produced remains visible in the history.
- **Traces:** rows 21, 33; M1

### WUI-CHAT-17: Escape aborts while streaming
- **Persona:** Keyboard-driven user
- **Preconditions:** The assistant is actively streaming; the editor textarea has focus
- **Steps:**
  1. Press the Escape key.
- **Assertions:**
  - A1: The current turn is aborted, identical to clicking the stop button.
  - A2: The send button reappears.
- **Traces:** row 21; M1

### WUI-CHAT-18: Escape does nothing when not streaming
- **Persona:** Keyboard-driven user composing a message
- **Preconditions:** No turn is in progress; the editor has typed text
- **Steps:**
  1. Press the Escape key.
- **Assertions:**
  - A1: No abort is triggered (there is nothing to abort).
  - A2: The typed text in the editor is preserved.
- **Traces:** row 21; M1

### WUI-CHAT-19: Per-turn cost stats bar shows accumulated usage
- **Persona:** Cost-conscious user tracking spend
- **Preconditions:** At least one assistant turn has completed with reported usage
- **Steps:**
  1. Read the bottom stats bar after a turn completes.
- **Assertions:**
  - A1: A usage summary (tokens/cost figures) is displayed at the right of the stats bar.
  - A2: After a second turn, the displayed totals reflect the sum across all completed assistant turns.
- **Traces:** row 20; M1

### WUI-CHAT-20: Clicking the cost stats opens the cost detail (when wired)
- **Persona:** User who wants a cost breakdown
- **Preconditions:** A cost-click handler is wired; usage totals are shown in the stats bar
- **Steps:**
  1. Hover the usage figure (it shows a pointer cursor and highlights).
  2. Click the usage figure.
- **Assertions:**
  - A1: The usage figure shows it is interactive (pointer cursor / hover color change).
  - A2: Clicking invokes the cost detail action (e.g., opens the session cost view).
- **Traces:** row 20; M1

### WUI-CHAT-21: Enter sends the message
- **Persona:** User composing a single-line prompt
- **Preconditions:** A model is selected; the editor contains non-empty text; no turn is in progress
- **Steps:**
  1. Type a message.
  2. Press Enter (without Shift).
- **Assertions:**
  - A1: The message is sent; the user bubble appears in history.
  - A2: The editor textarea is cleared after sending.
- **Traces:** rows 31, 33; M1

### WUI-CHAT-22: Shift+Enter inserts a newline instead of sending
- **Persona:** User writing a multi-line prompt
- **Preconditions:** The editor has focus with some text
- **Steps:**
  1. Type a line of text.
  2. Press Shift+Enter.
  3. Type a second line.
- **Assertions:**
  - A1: A newline is inserted; nothing is sent.
  - A2: The editor now contains both lines of text.
- **Traces:** row 31; M1

### WUI-CHAT-23: IME composition guard — Enter while composing does not send
- **Persona:** User typing CJK text via an IME
- **Preconditions:** An IME composition session is active in the editor (e.g., composing Chinese characters)
- **Steps:**
  1. Begin composing characters with the IME.
  2. Press Enter to confirm/commit the IME candidate (while composition is active).
- **Assertions:**
  - A1: The message is NOT sent; Enter is consumed by the IME to confirm the candidate.
  - A2: The composed text remains in the editor.
  - A3: A subsequent Enter (after composition has ended) sends normally.
- **Traces:** row 31; M1

### WUI-CHAT-24: Empty/whitespace-only message cannot be sent
- **Persona:** User who accidentally hits Enter on an empty editor
- **Preconditions:** The editor is empty or contains only whitespace; no attachments
- **Steps:**
  1. Press Enter with an empty editor.
  2. Observe the send button.
- **Assertions:**
  - A1: No message is sent and no user bubble is added.
  - A2: The send button is disabled while the editor is empty and has no attachments.
- **Traces:** rows 31, 33; M1

### WUI-CHAT-25: Textarea auto-grows up to a 200px cap then scrolls
- **Persona:** User pasting a long multi-line prompt
- **Preconditions:** The editor is empty and showing a single visible row
- **Steps:**
  1. Type or paste enough lines to exceed the visible height.
- **Assertions:**
  - A1: The textarea grows taller as more lines are added.
  - A2: Growth stops at a maximum height of 200px.
  - A3: Beyond 200px the textarea becomes internally scrollable rather than growing further.
- **Traces:** row 30; M1

### WUI-CHAT-26: Send/stop toggle reflects streaming state
- **Persona:** User observing editor controls during a turn
- **Preconditions:** The editor has sendable text; no turn in progress
- **Steps:**
  1. Send the message and watch the right-side control through the turn lifecycle.
- **Assertions:**
  - A1: Before sending, a send (arrow) button is shown.
  - A2: While the turn streams, the control is a stop (square) button.
  - A3: When the turn ends, it reverts to the send button.
- **Traces:** row 33; M1

### WUI-CHAT-27: Model-id button is shown and opens the model selector
- **Persona:** User who wants to confirm or change the active model
- **Preconditions:** A model is selected and the model selector is enabled
- **Steps:**
  1. Read the right toolbar of the editor.
  2. Click the model-id button.
- **Assertions:**
  - A1: The right toolbar shows a button labelled with the active model's id.
  - A2: Clicking it opens the model selector dialog.
  - A3: The dialog does not immediately close from the same click that opened it.
- **Traces:** rows 33; M1

### WUI-CHAT-28: Thinking-level selector appears only for reasoning-capable models
- **Persona:** User switching between a reasoning model and a non-reasoning model
- **Preconditions:** The thinking selector is enabled; two models are available — one with reasoning support and one without
- **Steps:**
  1. Select a reasoning-capable model and inspect the editor's left toolbar.
  2. Switch to a non-reasoning model and inspect again.
- **Assertions:**
  - A1: With a reasoning-capable model, a thinking-level selector is shown in the left toolbar.
  - A2: With a non-reasoning model, the thinking-level selector is absent.
  - A3: The selector offers Off, Minimal, Low, Medium, and High levels.
- **Traces:** row 32; M1

### WUI-CHAT-29: Changing the thinking level updates the active level
- **Persona:** Power user tuning reasoning effort
- **Preconditions:** A reasoning-capable model is active; the thinking selector currently reads "Off"
- **Steps:**
  1. Open the thinking-level selector and choose "High".
- **Assertions:**
  - A1: The selector now displays "High".
  - A2: The new level is applied to the session (subsequent turns use it).
- **Traces:** row 32; M1

### WUI-CHAT-30: Paperclip opens a file picker and shows a loading spinner during ingest
- **Persona:** User attaching a document
- **Preconditions:** Attachments are enabled; the editor's left toolbar shows a paperclip button
- **Steps:**
  1. Click the paperclip button and select a file.
  2. Observe the toolbar while the file is being ingested.
- **Assertions:**
  - A1: A native file picker opens when the paperclip is clicked.
  - A2: While the file is being processed, a spinning loader replaces the paperclip button.
  - A3: After ingestion completes, the attachment appears as a tile above the textarea and the paperclip returns.
- **Traces:** row 32; M1 (editor shell); M6 (ingest)

### WUI-CHAT-31: Send is enabled by attachments even with empty text
- **Persona:** User who wants to send only a file
- **Preconditions:** The editor text is empty; one attachment tile is present
- **Steps:**
  1. With empty text but an attachment present, observe the send button and press Enter.
- **Assertions:**
  - A1: The send button is enabled despite the empty text.
  - A2: Pressing Enter sends the message (the attachment is included).
- **Traces:** rows 31, 33; M1

### WUI-CHAT-32: Sending is blocked while a turn is already streaming
- **Persona:** Impatient user trying to send a second prompt mid-turn
- **Preconditions:** A turn is currently streaming
- **Steps:**
  1. Type a new message and press Enter while the assistant is still streaming.
- **Assertions:**
  - A1: The Enter key does not start a second send while streaming.
  - A2: The editor still shows the stop button (not send) while the turn is in progress.
- **Traces:** rows 21, 31, 33; M1

### WUI-CHAT-33: Multi-turn history renders stably with keyed messages
- **Persona:** User holding a back-and-forth conversation
- **Preconditions:** Several user/assistant turns have completed
- **Steps:**
  1. Send several messages, letting each turn complete before the next.
  2. Scroll through the full transcript.
- **Assertions:**
  - A1: All prior user and assistant messages remain in order in the history.
  - A2: Earlier messages are not visibly re-rendered or flicker when a new turn streams (stable keyed list).
  - A3: Each message appears exactly once.
- **Traces:** rows 24, 27; M1

### WUI-CHAT-34: Tool results pair to their tool calls in history
- **Persona:** User reviewing a completed turn that used a tool
- **Preconditions:** A completed turn included an assistant tool call and its result
- **Steps:**
  1. Scroll to the completed tool interaction in history.
- **Assertions:**
  - A1: The tool result is shown attached to / paired with its originating tool call (matched by call id).
  - A2: There is no orphaned tool-result card detached from its tool call.
- **Traces:** rows 24, 25; M1

### WUI-CHAT-35: Artifact-role messages are not shown as chat bubbles
- **Persona:** User whose conversation created artifacts
- **Preconditions:** The conversation includes artifact create/update actions (artifact-role messages persist for UI state)
- **Steps:**
  1. Scroll through the chat history after artifacts have been created.
- **Assertions:**
  - A1: No raw "artifact" entries appear as message bubbles in the chat history.
  - A2: The created artifacts are reachable only via the artifacts panel / floating pill, not as chat messages.
- **Traces:** row 25; M1

### WUI-CHAT-36: Hidden pending tool calls re-appear after the turn completes
- **Persona:** User watching a tool-using turn from start to finish
- **Preconditions:** A prompt triggers a tool call that runs and finishes within the turn
- **Steps:**
  1. Observe the tool card while pending (during streaming).
  2. Wait for the turn to fully complete and observe again.
- **Assertions:**
  - A1: During streaming, the pending tool card is shown only by the streaming area (hidden in the stable list).
  - A2: After the turn ends, the finished tool card is present in the stable list exactly once.
  - A3: At no point are two copies of the same tool card visible simultaneously.
- **Traces:** rows 26, 27; M1

### WUI-CHAT-37: Window resize across the 800px breakpoint switches layout modes
- **Persona:** User resizing their browser window
- **Preconditions:** Artifacts exist and the panel is open; the window starts wider than 800px
- **Steps:**
  1. Narrow the window from above 800px to below 800px.
  2. Widen it back above 800px.
- **Assertions:**
  - A1: Below 800px, the artifacts panel becomes a full-screen overlay over the chat.
  - A2: Above 800px, the artifacts panel returns to a side-by-side half-width column.
  - A3: The transition happens live on resize without a manual reload.
- **Traces:** row 12; M1

### WUI-CHAT-38: Prior text is preserved when a send is gated/canceled
- **Persona:** User whose send is blocked by an API-key prompt they cancel
- **Preconditions:** API-key gating is wired and reports a missing key for the selected model's provider
- **Steps:**
  1. Type a message and press Enter.
  2. When prompted to supply an API key, cancel/decline the prompt.
- **Assertions:**
  - A1: The message is not sent.
  - A2: The typed text remains in the editor (it is cleared only once a send is actually committed).
- **Traces:** rows 22, 23; M1
