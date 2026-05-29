# Image Delivery & Session Replay — Web UI Acceptance Test Cases

Scope: end-user behavior of attaching images and documents and getting them to the model; restoring a persisted session into both the browser transcript and the server-side context so follow-ups recall earlier facts; round-tripping tool-bearing assistant turns; new-session clearing both views; active-model hydration on connect; and switching the model mid-session. Cases tagged **[vision model required]** must be run against a vision-capable model so the assertions are observable; cases without that tag work with any model.

### WUI-CTX-01: Attaching a solid-color image lets a vision model name the color [vision model required]
- **Persona:** Designer pasting a swatch to check it
- **Preconditions:** A vision-capable model is active; the editor accepts attachments; a single solid-red PNG (well under 20MB) is ready to attach
- **Steps:**
  1. Attach the solid-red PNG to the editor.
  2. Type "What color is this image?" and send.
  3. Wait for the assistant turn to complete.
- **Assertions:**
  - A1: The assistant's reply names the color red (e.g. contains "red").
  - A2: The reply demonstrates the image content reached the model (it does not say it received no image or cannot see images).
  - A3: The attached image is shown in the sent user message in the transcript.
- **Traces:** Image delivery; final closeout (red PNG → "Red"); M10

### WUI-CTX-02: A single attached image is inlined in the prompt frame, not dropped
- **Persona:** User sending one screenshot with a question
- **Preconditions:** Any model is active; one image attachment is ready (a few hundred KB)
- **Steps:**
  1. Attach the image and send a prompt referencing it.
- **Assertions:**
  - A1: The outbound prompt carries the image inline (the image's base64 data and its MIME type travel in the prompt's images array).
  - A2: The image is not uploaded out-of-band and is not referenced by id only.
  - A3: The turn proceeds normally (a streaming assistant reply appears).
- **Traces:** sendMessage hybrid image inlining; M10

### WUI-CTX-03: Multiple attached images all reach the model in one prompt [vision model required]
- **Persona:** User comparing two pictures
- **Preconditions:** A vision-capable model is active; two distinct solid-color images are ready (e.g. one blue, one green)
- **Steps:**
  1. Attach both images to the same message.
  2. Type "Name the color of each image in order." and send.
- **Assertions:**
  - A1: Every attached image is inlined in the single prompt frame (none is silently dropped).
  - A2: The assistant names both colors (blue and green), confirming both images were delivered.
  - A3: Both images appear in the sent user message bubble.
- **Traces:** sendMessage "inline every image"; M10

### WUI-CTX-04: A large image up to the 20MB cap is still inlined and delivered [vision model required]
- **Persona:** User attaching a high-resolution photo
- **Preconditions:** A vision-capable model is active; an image attachment near (but not over) the 20MB per-attachment cap is ready
- **Steps:**
  1. Attach the large image and send a prompt asking the model to describe it.
- **Assertions:**
  - A1: The large image is inlined in the prompt frame (it is not switched to upload-by-reference because of its size).
  - A2: The connection delivers the frame without error and the assistant responds describing the image.
  - A3: A file that exceeds the 20MB attachment cap is rejected/blocked by the editor before send (the oversized attachment never reaches the prompt).
- **Traces:** "bounded by the editor's 20MB attachment cap"; M10

### WUI-CTX-05: A document attachment delivers its extracted text inline in the message
- **Persona:** Analyst dropping a text document and asking about it
- **Preconditions:** Any model is active; a document attachment whose text was successfully extracted in the browser is ready
- **Steps:**
  1. Type "Summarize the attached document." and attach the document.
  2. Send the message.
- **Assertions:**
  - A1: The message the agent receives includes the document's extracted text, prefixed by a per-file header naming the document file.
  - A2: The assistant's summary reflects the actual document content (it does not claim the document was empty or missing).
  - A3: The user's own typed prompt text still precedes the appended document text.
- **Traces:** "Documents ... extracted text appended"; convert.ts convertAttachments; M10

### WUI-CTX-06: A document's text is delivered even when its binary was uploaded out-of-band
- **Persona:** User attaching a larger non-image file with extractable text
- **Preconditions:** Any model is active; a non-image document with extracted text is ready; the upload endpoint is reachable
- **Steps:**
  1. Attach the document and send a prompt about it.
- **Assertions:**
  - A1: The non-image binary is uploaded out-of-band and carried as a lightweight reference (id, file name, MIME type, size) in the frame's attachments, keeping the frame small.
  - A2: Independently of the upload, the document's extracted text is appended to the message so the agent receives the content directly.
  - A3: The assistant's answer uses the document content, proving the text path delivered it.
- **Traces:** "deliver via appended extracted text even when uploaded"; M10

### WUI-CTX-07: A failed out-of-band upload still delivers the document's text
- **Persona:** User on a flaky network attaching a document
- **Preconditions:** Any model is active; a document with extracted text is attached; the upload endpoint will fail for this attempt
- **Steps:**
  1. Attach the document and send a prompt about it.
- **Assertions:**
  - A1: The send does not error out to the user despite the upload failure.
  - A2: The assistant's answer still reflects the document content (the appended extracted text carried it).
  - A3: The turn completes normally with an assistant reply.
- **Traces:** sendMessage upload-failure fallback (documents already carry text); M10

### WUI-CTX-08: A message with text plus an image plus a document delivers all three parts
- **Persona:** Power user combining a question, a screenshot, and a spec file
- **Preconditions:** A vision-capable model is active; one image attachment and one text-extractable document are attached together with typed prompt text
- **Steps:**
  1. Type a question, attach both the image and the document, and send.
- **Assertions:**
  - A1: The typed text is delivered as the message body.
  - A2: The image is inlined in the prompt frame's images.
  - A3: The document's extracted text is appended to the message body under its file header.
  - A4: The assistant's reply shows awareness of the question, the image, and the document content.
- **Traces:** sendMessage hybrid dispatch (images + documentTexts + references); M10

### WUI-CTX-09: Loading a persisted session restores the browser transcript
- **Persona:** Returning user reopening yesterday's conversation
- **Preconditions:** A persisted session exists in browser storage with several user/assistant turns, a saved model, and a saved thinking level
- **Steps:**
  1. Open/restore the persisted session.
- **Assertions:**
  - A1: The full transcript reappears in the chat history in original order.
  - A2: The editor reflects the session's saved model and thinking level.
  - A3: The input is re-enabled and the view is not in a streaming state.
- **Traces:** loadSession (replace displayed transcript); M10

### WUI-CTX-10: A follow-up after loading a session recalls earlier facts (server replay)
- **Persona:** User continuing a saved conversation expecting it to remember context
- **Preconditions:** A persisted session exists in which the user earlier stated a lucky number (e.g. 47); that session is just loaded
- **Steps:**
  1. Load the persisted session.
  2. Send a new prompt: "What's my lucky number?"
  3. Wait for the reply.
- **Assertions:**
  - A1: The assistant answers with the previously stated value (47), proving the transcript was replayed into the server context.
  - A2: The answer is correct even though this is a fresh server connection that never saw the original turn live.
  - A3: The recalled fact comes from the restored history, not from the user re-stating it in step 2.
- **Traces:** loadSession set_messages → server set_messages; final closeout ("lucky number 47"); M10

### WUI-CTX-11: Loading a session applies the saved model to the server before the next turn
- **Persona:** User whose saved session used a specific model
- **Preconditions:** A persisted session whose saved model differs from the server's current model is available
- **Steps:**
  1. Load the persisted session.
  2. Send a follow-up prompt.
- **Assertions:**
  - A1: After load, the editor shows the session's saved model.
  - A2: The follow-up turn runs under the saved model (the server was told to switch before the prompt).
- **Traces:** loadSession set_model; M10

### WUI-CTX-12: A tool-bearing assistant turn round-trips through save and reload
- **Persona:** User reopening a session where the assistant previously called a tool
- **Preconditions:** A persisted session whose history contains an assistant turn with a tool call and its tool result is available
- **Steps:**
  1. Load the persisted session.
  2. Send a follow-up that depends on the earlier tool interaction (e.g. "What did that tool return?").
- **Assertions:**
  - A1: The restored transcript renders the earlier tool call and tool result without error.
  - A2: The follow-up reply reflects the earlier tool interaction, proving the tool-bearing assistant turn replayed into the server context.
  - A3: No deserialize/parse error or dropped-turn warning is surfaced to the user during load.
- **Traces:** toolCall→toolcall de-normalization (denormalizeBlock/denormalizeMessage); M10

### WUI-CTX-13: UI-only message roles are skipped server-side on replay without error
- **Persona:** User reloading a session that contains UI-only entries (e.g. artifact placeholders)
- **Preconditions:** A persisted session whose history mixes model-native roles (user/assistant/toolResult) with UI-only roles the browser layered on is available
- **Steps:**
  1. Load the persisted session.
  2. Send a follow-up prompt that relies on the model-native history.
- **Assertions:**
  - A1: The load completes without any error, despite the presence of UI-only roles.
  - A2: Only model-native roles are replayed into the server context; UI-only entries are skipped server-side rather than rejected.
  - A3: The follow-up still recalls facts from the model-native turns, confirming those replayed correctly.
- **Traces:** loadSession role filter; server set_messages "entries that fail to deserialize are skipped"; M10

### WUI-CTX-14: New session clears the browser view
- **Persona:** User starting a clean conversation
- **Preconditions:** An active conversation with several turns is on screen
- **Steps:**
  1. Trigger "new session".
- **Assertions:**
  - A1: The chat history is emptied (no prior bubbles, tool cards, or streaming cursor remain).
  - A2: The editor returns to an empty, ready state with input enabled.
  - A3: Any in-progress streaming/pending state is reset.
- **Traces:** newSession (clear local state); M10

### WUI-CTX-15: New session clears the server context so follow-ups no longer recall prior facts
- **Persona:** User who wants the model to forget the previous conversation
- **Preconditions:** An active conversation in which the user stated a fact (e.g. "my lucky number is 47") and the model already acknowledged it
- **Steps:**
  1. Trigger "new session".
  2. Send "What's my lucky number?"
- **Assertions:**
  - A1: The assistant does not return the previously stated value; it indicates it does not know.
  - A2: The earlier fact is absent from the server context, confirming new-session reset both views.
  - A3: The new turn runs on a fresh, empty conversation.
- **Traces:** newSession → server new_session (reset_session clears context); M10

### WUI-CTX-16: Model hydration on connect shows the server's real active model
- **Persona:** User connecting to a server started on a non-default model
- **Preconditions:** The server was started with a non-default model (e.g. a vision model such as gemini-2.5-flash); the UI boots with a placeholder model label
- **Steps:**
  1. Open the app and let the WebSocket connect.
- **Assertions:**
  - A1: After connect, the editor's model selector displays the server's actual active model, not the bootstrap placeholder.
  - A2: A turn sent immediately after connect runs under that server model.
- **Traces:** hydrate() get_state → set active model; final closeout (server on gemini-2.5-flash shows gemini-2.5-flash); M10

### WUI-CTX-17: Hydration failure leaves the placeholder label intact (best-effort)
- **Persona:** User connecting when the state query is briefly unavailable
- **Preconditions:** The connection is established but the state query does not return a model (transient failure)
- **Steps:**
  1. Open the app and let the connection settle.
- **Assertions:**
  - A1: The app does not crash or show an error dialog because hydration failed.
  - A2: The editor keeps its bootstrap placeholder model label rather than blanking out.
  - A3: The user can still send a prompt.
- **Traces:** hydrate() best-effort catch; M10

### WUI-CTX-18: Switching the model mid-session takes effect on the next turn
- **Persona:** User changing models partway through a conversation
- **Preconditions:** A conversation is in progress under model A; model B is available in the selector
- **Steps:**
  1. Pick model B in the model selector.
  2. Send a follow-up prompt.
- **Assertions:**
  - A1: The editor immediately reflects model B as active.
  - A2: The follow-up turn runs under model B (the server was told to switch).
  - A3: The existing transcript and conversation context are preserved across the switch (history is not cleared).
- **Traces:** setModel → server set_model; M8/M10

### WUI-CTX-19: Switching to a vision model lets a previously-attached image be discussed [vision model required]
- **Persona:** User who realizes a non-vision model can't see their image and switches
- **Preconditions:** A conversation exists where an image was attached in an earlier turn; the current model is non-vision; a vision-capable model is available
- **Steps:**
  1. Switch to the vision-capable model.
  2. Re-attach (or reference) the image and ask the model to describe it.
- **Assertions:**
  - A1: After the switch, the editor shows the vision model active.
  - A2: The newly attached image is inlined in the prompt and the vision model describes it correctly.
  - A3: The conversation continues in the same session (prior turns remain visible).
- **Traces:** setModel + sendMessage image inlining; M8/M10

### WUI-CTX-20: Reload-then-continue preserves multi-turn memory across the whole replayed transcript
- **Persona:** User resuming a long saved conversation and probing several earlier details
- **Preconditions:** A persisted session storing multiple distinct facts across turns (e.g. lucky number 47 and favorite color teal) is available
- **Steps:**
  1. Load the persisted session.
  2. Ask "What's my lucky number and my favorite color?"
- **Assertions:**
  - A1: The assistant returns both facts (47 and teal), proving the full transcript — not just the last turn — replayed into the server context.
  - A2: The restored browser transcript shows all the original turns that stated those facts.
  - A3: No turn from the saved history is missing or duplicated after load.
- **Traces:** loadSession set_messages (full transcript); final closeout ("lucky number 47, favorite color teal"); M10
