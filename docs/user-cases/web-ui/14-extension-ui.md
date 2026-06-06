# Extension UI Protocol — Web UI Acceptance Test Cases

Scope: end-user behavior when a loaded extension drives the host UI. The server relays an extension's host-UI calls as inbound `extension_ui_request` frames; the web UI renders the matching modal dialog (`select` / `confirm` / `input` / `editor`) or applies the non-modal method (`notify` / `setStatus` / `setTitle` / `setWidget` / `set_editor_text`) directly, then — for the interactive methods — sends back exactly one `extension_ui_response` frame keyed by the request `id`, carrying exactly one of `value` / `confirmed` / `cancelled`. Dismissing a dialog (Escape / backdrop / Cancel) or an elapsed `timeout` resolves as `cancelled`. Cases are framed around a simulated inbound request frame and the resulting dialog plus the outbound reply.

### WUI-EXT-01: select dialog renders options from the request
- **Persona:** End user with a loaded extension that calls the host `select` UI
- **Preconditions:** A simulated inbound frame `{ "type": "extension_ui_request", "id": "s1", "method": "select", "title": "Pick a branch", "options": ["main", "develop", "release"] }`
- **Steps:**
  1. Deliver the frame to the open web UI.
  2. Observe the modal dialog that appears.
- **Assertions:**
  - A1: A single modal dialog opens, with a backdrop behind it.
  - A2: The dialog heading reads exactly "Pick a branch".
  - A3: Three clickable option rows are shown, labelled "main", "develop", and "release", in request order.
  - A4: A "Cancel" affordance is present.
  - A5: No `extension_ui_response` frame is sent yet (the dialog is still awaiting a choice).
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-02: choosing a select option replies with the chosen value
- **Persona:** End user answering an extension's single-choice prompt
- **Preconditions:** The `select` dialog from WUI-EXT-01 (id `s1`) is open
- **Steps:**
  1. Click the "develop" option row.
- **Assertions:**
  - A1: The dialog closes immediately.
  - A2: Exactly one outbound frame is sent: `{ "type": "extension_ui_response", "id": "s1", "value": "develop" }`.
  - A3: The outbound `id` equals the request `id` ("s1").
  - A4: The frame carries `value` only — no `confirmed` and no `cancelled` field.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-03: cancelling a select dialog replies cancelled
- **Persona:** End user who declines to pick from the extension's list
- **Preconditions:** A `select` request `{ id: "s2", method: "select", title: "Pick a file", options: ["a.txt", "b.txt"] }` has opened its dialog
- **Steps:**
  1. Click the "Cancel" affordance.
- **Assertions:**
  - A1: The dialog closes.
  - A2: Exactly one outbound frame is sent: `{ "type": "extension_ui_response", "id": "s2", "cancelled": true }`.
  - A3: The frame carries `cancelled: true` only — no `value`, no `confirmed`.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-04: select with an empty options list shows an empty-state and is cancellable
- **Persona:** End user receiving a `select` request that carries no options
- **Preconditions:** A request `{ id: "s3", method: "select", title: "Pick one", options: [] }`
- **Steps:**
  1. Deliver the frame and observe the dialog body.
  2. Dismiss the dialog with the Cancel affordance.
- **Assertions:**
  - A1: The dialog opens with heading "Pick one".
  - A2: An empty-state message indicating no options are available is shown instead of option rows.
  - A3: No selectable option rows are present.
  - A4: After Cancel, the reply is `{ "type": "extension_ui_response", "id": "s3", "cancelled": true }`.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-05: confirm dialog shows title and message with Yes/No
- **Persona:** End user prompted by an extension to confirm an action
- **Preconditions:** A request `{ id: "c1", method: "confirm", title: "Proceed?", message: "Continue with edits?" }`
- **Steps:**
  1. Deliver the frame and observe the dialog.
- **Assertions:**
  - A1: A modal dialog opens with heading "Proceed?".
  - A2: The body text reads exactly "Continue with edits?".
  - A3: Two action buttons are present, labelled "No" and "Yes".
  - A4: No reply frame has been sent yet.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-06: clicking Yes replies confirmed true; clicking No replies confirmed false
- **Persona:** End user answering an extension's yes/no question
- **Preconditions:** Two separate `confirm` requests are tested: `{ id: "c2", method: "confirm", title: "Save?", message: "Save now?" }` and `{ id: "c3", method: "confirm", title: "Delete?", message: "Delete now?" }`
- **Steps:**
  1. For c2's dialog, click "Yes".
  2. For c3's dialog, click "No".
- **Assertions:**
  - A1: c2 replies `{ "type": "extension_ui_response", "id": "c2", "confirmed": true }`.
  - A2: c3 replies `{ "type": "extension_ui_response", "id": "c3", "confirmed": false }`.
  - A3: Each reply carries `confirmed` only — no `value`, no `cancelled`.
  - A4: Each dialog closes after its button is clicked.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-07: dismissing a confirm dialog with Escape replies cancelled (not confirmed:false)
- **Persona:** End user who closes a confirm prompt without answering
- **Preconditions:** A `confirm` request `{ id: "c4", method: "confirm", title: "Overwrite?", message: "Overwrite the file?" }` with its dialog open
- **Steps:**
  1. Press the Escape key.
- **Assertions:**
  - A1: The dialog closes.
  - A2: The reply is `{ "type": "extension_ui_response", "id": "c4", "cancelled": true }`.
  - A3: The reply is NOT `confirmed: false` — Escape is treated as a cancellation, distinct from an explicit "No".
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-08: input dialog shows placeholder and an autofocused single-line field
- **Persona:** End user prompted by an extension to type a single-line value
- **Preconditions:** A request `{ id: "i1", method: "input", title: "Name the tag", placeholder: "v1.0.0" }`
- **Steps:**
  1. Deliver the frame and observe the dialog.
- **Assertions:**
  - A1: A modal dialog opens with heading "Name the tag".
  - A2: A single-line text field is shown, displaying placeholder text "v1.0.0" while empty.
  - A3: The text field receives focus automatically when the dialog appears.
  - A4: "Cancel" and "Submit" affordances are present.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-09: input Submit replies with the typed value
- **Persona:** End user entering a value for an extension
- **Preconditions:** The `input` dialog from WUI-EXT-08 (id `i1`) is open and focused
- **Steps:**
  1. Type "release-2" into the field.
  2. Click "Submit".
- **Assertions:**
  - A1: The dialog closes.
  - A2: The reply is `{ "type": "extension_ui_response", "id": "i1", "value": "release-2" }`.
  - A3: The reply carries `value` only.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-10: pressing Enter in the input field submits the value
- **Persona:** End user who prefers the keyboard for a single-line prompt
- **Preconditions:** An `input` request `{ id: "i2", method: "input", title: "Commit message", placeholder: "msg" }` with its dialog open and focused
- **Steps:**
  1. Type "fix typo".
  2. Press the Enter key (no modifier, not mid-IME-composition).
- **Assertions:**
  - A1: The dialog closes.
  - A2: The reply is `{ "type": "extension_ui_response", "id": "i2", "value": "fix typo" }`.
  - A3: Pressing Enter does not insert a newline into the field nor leave the dialog open.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-11: cancelling the input dialog replies cancelled and discards typed text
- **Persona:** End user who changes their mind after typing
- **Preconditions:** An `input` request `{ id: "i3", method: "input", title: "Enter value" }` with its dialog open; the user has typed "draft text"
- **Steps:**
  1. Click "Cancel".
- **Assertions:**
  - A1: The dialog closes.
  - A2: The reply is `{ "type": "extension_ui_response", "id": "i3", "cancelled": true }`.
  - A3: The typed text "draft text" is NOT sent as a `value`.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-12: editor dialog opens prefilled with a multiline editing area
- **Persona:** End user editing a block of text supplied by an extension
- **Preconditions:** A request `{ id: "e1", method: "editor", title: "Edit commit body", prefill: "line one\nline two" }`
- **Steps:**
  1. Deliver the frame and observe the dialog.
- **Assertions:**
  - A1: A modal dialog opens with heading "Edit commit body".
  - A2: A multiline editing area is shown, prefilled with the two lines "line one" and "line two".
  - A3: The editing area is taller than a single-line input and accepts newlines.
  - A4: "Cancel" and "Submit" affordances are present.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-13: editor Submit replies with the full edited multiline value
- **Persona:** End user finishing a multiline edit for an extension
- **Preconditions:** The `editor` dialog from WUI-EXT-12 (id `e1`) is open with prefill "line one\nline two"
- **Steps:**
  1. Append a third line so the content becomes "line one\nline two\nline three".
  2. Click "Submit".
- **Assertions:**
  - A1: The dialog closes.
  - A2: The reply is `{ "type": "extension_ui_response", "id": "e1", "value": "line one\nline two\nline three" }`.
  - A3: Internal newlines are preserved verbatim in `value`.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-14: cancelling the editor dialog replies cancelled
- **Persona:** End user who abandons a multiline edit
- **Preconditions:** An `editor` request `{ id: "e2", method: "editor", title: "Notes", prefill: "keep me" }` with its dialog open and edits in progress
- **Steps:**
  1. Click "Cancel".
- **Assertions:**
  - A1: The dialog closes.
  - A2: The reply is `{ "type": "extension_ui_response", "id": "e2", "cancelled": true }`.
  - A3: No `value` is sent even though the field held text.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-15: dismissing a dialog via backdrop click replies cancelled
- **Persona:** End user who clicks outside the dialog to dismiss it
- **Preconditions:** Any interactive request open as a dialog, e.g. `{ id: "b1", method: "input", title: "Quick value" }`
- **Steps:**
  1. Click the dimmed backdrop area outside the dialog box.
- **Assertions:**
  - A1: The dialog closes.
  - A2: The reply is `{ "type": "extension_ui_response", "id": "b1", "cancelled": true }`.
  - A3: Exactly one reply frame is sent for the request.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-16: a request timeout auto-resolves as cancelled with no user action
- **Persona:** End user who walks away from an extension prompt
- **Preconditions:** A request carrying a timeout, e.g. `{ id: "t1", method: "confirm", title: "Still there?", message: "Confirm within the window", timeout: 1000 }`; the dialog is open and untouched
- **Steps:**
  1. Take no action and let the timeout window elapse.
- **Assertions:**
  - A1: After the timeout elapses, the dialog closes on its own.
  - A2: The reply is `{ "type": "extension_ui_response", "id": "t1", "cancelled": true }` — an elapsed timeout resolves as cancelled, not confirmed.
  - A3: Exactly one reply frame is sent (the timeout fires only once).
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-17: answering before the timeout sends the answer and suppresses the timeout
- **Persona:** End user who responds quickly to a timed prompt
- **Preconditions:** A request `{ id: "t2", method: "input", title: "Fast answer", placeholder: "type", timeout: 5000 }` with its dialog open
- **Steps:**
  1. Type "answered" and click "Submit" well before the timeout would elapse.
  2. Wait past the original timeout window.
- **Assertions:**
  - A1: The reply `{ "type": "extension_ui_response", "id": "t2", "value": "answered" }` is sent at submit time.
  - A2: No second reply (no later `cancelled`) is sent after the timeout window passes.
  - A3: Exactly one reply frame total is associated with id "t2".
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-18: notify renders an info / warning / error toast and sends no reply
- **Persona:** End user receiving non-modal notices from an extension
- **Preconditions:** Three requests delivered in turn: `{ id: "n1", method: "notify", message: "Build started" }` (no `notifyType`), `{ id: "n2", method: "notify", message: "Low disk space", notifyType: "warning" }`, `{ id: "n3", method: "notify", message: "Build failed", notifyType: "error" }`
- **Steps:**
  1. Deliver each notify frame.
  2. Observe the toast area.
- **Assertions:**
  - A1: Three transient toasts appear (no modal dialog or backdrop for any of them).
  - A2: The toasts read "Build started", "Low disk space", and "Build failed" respectively.
  - A3: The error toast ("Build failed") is styled distinctly (destructive emphasis) from the info/warning toasts; the default-typed n1 is treated as info.
  - A4: No `extension_ui_response` frame is sent for any notify request (non-modal methods expect no reply).
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-19: a toast auto-dismisses after its lifetime and can be clicked to dismiss early
- **Persona:** End user managing transient extension notices
- **Preconditions:** Two info notifies delivered: `{ id: "n4", method: "notify", message: "Stays then fades" }` and `{ id: "n5", method: "notify", message: "Click to close" }`
- **Steps:**
  1. Click the "Click to close" toast.
  2. Leave the "Stays then fades" toast untouched and wait past its display lifetime.
- **Assertions:**
  - A1: The clicked toast is removed immediately on click.
  - A2: The untouched toast disappears on its own after its display lifetime elapses.
  - A3: Dismissing a toast (by click or expiry) sends no `extension_ui_response` frame.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-20: setStatus shows a labelled status toast; a cleared status is a no-op
- **Persona:** End user watching an extension's status updates
- **Preconditions:** Two requests: `{ id: "st1", method: "setStatus", statusKey: "lint", statusText: "running" }` and a cleared one `{ id: "st2", method: "setStatus", statusKey: "lint", statusText: null }`
- **Steps:**
  1. Deliver st1, then st2.
- **Assertions:**
  - A1: st1 produces a transient toast whose text combines the key and text, reading "lint: running".
  - A2: st2 (cleared status, no text) produces no toast — it is a no-op.
  - A3: Neither setStatus request triggers a modal dialog nor an `extension_ui_response` frame.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-21: setTitle updates the app header title
- **Persona:** End user whose extension relabels the app surface
- **Preconditions:** The app header shows its current title; a request `{ id: "ti1", method: "setTitle", title: "Release Console" }`
- **Steps:**
  1. Deliver the setTitle frame.
- **Assertions:**
  - A1: The app header title updates to read "Release Console".
  - A2: No modal dialog or toast is shown for this request.
  - A3: No `extension_ui_response` frame is sent.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-22: setWidget summarizes its lines as a toast; an empty/cleared widget shows nothing
- **Persona:** End user receiving a multi-line widget summary from an extension
- **Preconditions:** Two requests: `{ id: "w1", method: "setWidget", widgetKey: "status", widgetLines: ["Tests: 12 passed", "Coverage: 87%"] }` and a cleared one `{ id: "w2", method: "setWidget", widgetKey: "status", widgetLines: null }`
- **Steps:**
  1. Deliver w1, then w2.
- **Assertions:**
  - A1: w1 produces a single transient toast summarizing both lines, with "Tests: 12 passed" and "Coverage: 87%" each on its own line.
  - A2: w2 (null / empty lines) produces no toast.
  - A3: Neither setWidget request opens a modal dialog nor sends an `extension_ui_response` frame.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-23: set_editor_text surfaces a suggested-input hint toast
- **Persona:** End user whose extension suggests text for the chat editor
- **Preconditions:** A request `{ id: "se1", method: "set_editor_text", text: "git push origin main" }`
- **Steps:**
  1. Deliver the set_editor_text frame.
- **Assertions:**
  - A1: A transient hint toast appears referencing the suggested input and containing the text "git push origin main".
  - A2: No modal dialog is shown.
  - A3: No `extension_ui_response` frame is sent (this is a non-modal method).
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-24: multiple sequential interactive requests each reply keyed by their own id
- **Persona:** End user working through a short extension wizard
- **Preconditions:** Three interactive requests arrive in sequence, each answered before the next is delivered: `{ id: "q1", method: "confirm", title: "Start?", message: "Begin?" }`, then `{ id: "q2", method: "input", title: "Name", placeholder: "name" }`, then `{ id: "q3", method: "select", title: "Env", options: ["dev", "prod"] }`
- **Steps:**
  1. For q1, click "Yes".
  2. For q2, type "demo" and Submit.
  3. For q3, click "prod".
- **Assertions:**
  - A1: q1 replies `{ "type": "extension_ui_response", "id": "q1", "confirmed": true }`.
  - A2: q2 replies `{ "type": "extension_ui_response", "id": "q2", "value": "demo" }`.
  - A3: q3 replies `{ "type": "extension_ui_response", "id": "q3", "value": "prod" }`.
  - A4: Each reply's `id` matches its originating request; replies are not cross-keyed or duplicated.
  - A5: Exactly three reply frames are sent in total, one per request.
- **Traces:** rows 188-189; §5.3; M9

### WUI-EXT-25: an unrecognized extension UI method is ignored without dialog or reply
- **Persona:** End user on a UI build older than the extension's protocol version
- **Preconditions:** A simulated frame with an unknown method, e.g. `{ "type": "extension_ui_request", "id": "x1", "method": "futureMethod", "payload": {} }`
- **Steps:**
  1. Deliver the frame.
- **Assertions:**
  - A1: No modal dialog opens and no toast appears.
  - A2: No `extension_ui_response` frame is sent for id "x1".
  - A3: The web UI remains responsive; subsequent valid extension UI requests still render normally.
- **Traces:** rows 188-189; §5.3; M9
