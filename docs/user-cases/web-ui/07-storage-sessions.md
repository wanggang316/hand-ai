# Storage & Sessions — Web UI Acceptance Test Cases

Scope: end-user acceptance for the browser persistence layer — conversation auto-save and reload-survival, the IndexedDB database (`hand-ai`) and its object stores, app settings persistence (theme + document-fetch proxy), per-provider API-key presence tracking that never leaks values, the dual-store sessions design (atomic save/delete/rename, newest-first listing, latest-session lookup), custom-provider CRUD, quota and persistent-storage reporting with graceful fallback, and resilient degradation when data is missing, corrupt, or a write fails. All cases are observable from the running web UI by a non-technical end user (browser DevTools "Application > IndexedDB" panel is used only to confirm what the UI already implies).

---

### WUI-STO-01: Conversation auto-saves on turn completion and survives a reload
- **Persona:** A returning user who expects today's chat to still be there after the browser refreshes.
- **Preconditions:** App open at the printed `http://127.0.0.1:<port>` URL; a working model + API key configured; no errors in the console.
- **Steps:**
  1. Type "Summarize the plot of Hamlet in two sentences" and send it.
  2. Wait for the assistant to finish streaming (the turn completes).
  3. Reload the page (browser refresh).
  4. Open the sessions list from the header and select the just-finished conversation.
- **Assertions:**
  - A1: After the turn completes, no auto-save error toast or red console error appears.
  - A2: After reload, the sessions list contains an entry whose preview text matches the start of the conversation.
  - A3: Selecting that entry restores the full transcript: the original user message and the complete assistant reply are both visible, in order.
  - A4: The transcript shown after reload is identical in message count to what was on screen before the reload.
- **Traces:** matrix-136; matrix-131; M7

### WUI-STO-02: Restored session keeps its model and thinking level
- **Persona:** A user who configured a specific model and reasoning depth and wants those preserved when reopening the chat.
- **Preconditions:** At least two selectable models available; a saved session exists from an earlier turn run with a non-default model and a non-"off" thinking level.
- **Steps:**
  1. Note the active model name and the thinking-level shown before reload.
  2. Reload the page.
  3. Open the sessions list and load the saved session.
- **Assertions:**
  - A1: After loading, the active model indicator shows the same model that was active when the session was saved.
  - A2: The thinking-level control reflects the same level that was saved (not reset to the default).
  - A3: Sending a new message in the restored session uses the restored model/level without the user re-selecting them.
- **Traces:** matrix-136; matrix-134; M7

### WUI-STO-03: The `hand-ai` IndexedDB database and its object stores exist
- **Persona:** A privacy-conscious user verifying where their chats are stored locally.
- **Preconditions:** App has been opened at least once and one conversation turn has completed.
- **Steps:**
  1. Open browser DevTools and navigate to Application > IndexedDB.
  2. Expand the database named `hand-ai`.
- **Assertions:**
  - A1: A database named exactly `hand-ai` is present.
  - A2: It contains the object stores: `settings`, `provider-keys`, `sessions`, `sessions-metadata`, and `custom-providers`.
  - A3: The `sessions` store holds at least one record after a completed turn, keyed by a session id.
  - A4: No second/duplicate database with a different name was created for normal app use.
- **Traces:** matrix-126; matrix-135; M7

### WUI-STO-04: Empty conversation is not persisted as a phantom session
- **Persona:** A user who opens the app, looks around, but never sends a message.
- **Preconditions:** Fresh app load.
- **Steps:**
  1. Open the app and do not send any message.
  2. Reload the page.
  3. Open the sessions list.
- **Assertions:**
  - A1: No empty/blank session was created from the idle visit.
  - A2: The sessions list shows only sessions that actually contain messages (or is empty if none ever had messages).
- **Traces:** matrix-136; M7

### WUI-STO-05: Theme preference persists across reload
- **Persona:** A user who prefers light mode and wants it to stick.
- **Preconditions:** App open in the default (dark) theme.
- **Steps:**
  1. Switch the theme to light using the header theme control.
  2. Reload the page.
- **Assertions:**
  - A1: Immediately after switching, the page visibly changes to the light theme.
  - A2: After reload, the page loads in light theme without the user toggling it again.
  - A3: Switching back to dark and reloading again restores dark — the last choice always wins.
- **Traces:** matrix-129; M7

### WUI-STO-06: Document-fetch proxy setting persists across reload
- **Persona:** A user behind a corporate network who configured a document-fetch proxy once.
- **Preconditions:** Settings dialog reachable from the header; Proxy tab available.
- **Steps:**
  1. Open Settings and go to the Proxy tab.
  2. Enter a proxy configuration value and save it.
  3. Reload the page.
  4. Reopen Settings > Proxy.
- **Assertions:**
  - A1: The proxy value entered is still shown after reload (read back from the `settings` store).
  - A2: No other setting (e.g. theme) was disturbed by saving the proxy value.
- **Traces:** matrix-129; M7

### WUI-STO-07: A provider key is stored and shown as present without revealing the value
- **Persona:** A user who pastes an API key once and never wants it shown back to them in clear text.
- **Preconditions:** Settings > API Keys (or the in-flow key prompt) reachable; a provider that requires a key.
- **Steps:**
  1. Open the API-key input for a provider and paste a valid-looking key.
  2. Save it.
  3. Reopen the same provider's key input.
  4. Reload the page and reopen it again.
- **Assertions:**
  - A1: After saving, the UI shows a "stored"/checkmark indicator for that provider.
  - A2: The actual key characters are NOT displayed back (no plaintext echo of the saved value).
  - A3: The "stored" indicator survives a page reload.
  - A4: The presence indicator is derived from a has/list check that reports only the provider name, never the secret value.
- **Traces:** matrix-130; M7

### WUI-STO-08: Provider-key presence list reports names only, never values
- **Persona:** A user with keys configured for several providers.
- **Preconditions:** Keys stored for two or more providers.
- **Steps:**
  1. Configure keys for provider A and provider B.
  2. Open the API Keys settings view.
  3. (Optional verification) Inspect the `provider-keys` store names in DevTools.
- **Assertions:**
  - A1: Both providers show a "key stored" state.
  - A2: The list of providers-with-keys includes A and B and excludes providers that have no key.
  - A3: The listing surface never renders the key strings — only provider identities and a present/absent state.
- **Traces:** matrix-130; M7

### WUI-STO-09: An API-key prompt is satisfied by a stored key and not shown again after reload
- **Persona:** A user prompted for a key mid-chat who does not want to be asked every visit.
- **Preconditions:** A provider that requires a key; no key stored yet for it.
- **Steps:**
  1. Send a message that triggers the "API key required" prompt for the provider.
  2. Enter a key in the prompt and confirm.
  3. Reload the page and send another message to the same provider.
- **Assertions:**
  - A1: The first send raises the key prompt.
  - A2: After entering the key, the send proceeds (the prompt resolves).
  - A3: After reload, sending to the same provider does NOT re-raise the prompt — the stored key's presence is detected first.
- **Traces:** matrix-130; matrix-136; M7

### WUI-STO-10: Removing a provider key flips the indicator back to "not stored"
- **Persona:** A user rotating credentials who deletes the old key.
- **Preconditions:** A provider with a stored key.
- **Steps:**
  1. Open the provider's key input showing the "stored" state.
  2. Clear/remove the stored key.
  3. Reload the page.
- **Assertions:**
  - A1: After removal, the indicator changes to "not stored".
  - A2: After reload, the provider still shows "not stored" (the deletion persisted).
  - A3: Other providers' keys are unaffected.
- **Traces:** matrix-130; M7

### WUI-STO-11: A saved session writes BOTH the full record and its list entry
- **Persona:** A user who expects a finished chat to appear in the session list AND be fully reopenable.
- **Preconditions:** A completed conversation turn.
- **Steps:**
  1. Complete a turn so the session auto-saves.
  2. Open the sessions list (driven by metadata).
  3. Load the session (driven by full data).
- **Assertions:**
  - A1: The session appears in the list (a `sessions-metadata` record exists).
  - A2: Loading it reconstructs the full transcript (a `sessions` data record exists).
  - A3: Both records share the same session id (list entry and opened conversation refer to the same session).
  - A4: The list entry's message count matches the number of messages restored on load.
- **Traces:** matrix-131; M7

### WUI-STO-12: Sessions list is ordered newest-first
- **Persona:** A user who wants their most recent conversation at the top.
- **Preconditions:** None.
- **Steps:**
  1. Start a new session and complete a turn ("First chat").
  2. Start another new session and complete a turn ("Second chat").
  3. Start a third new session and complete a turn ("Third chat").
  4. Reload the page and open the sessions list.
- **Assertions:**
  - A1: The list orders entries by last-modified descending: Third, then Second, then First.
  - A2: Returning to an older session and completing a new turn moves it to the top of the list (its last-modified advances).
- **Traces:** matrix-132; M7

### WUI-STO-13: The most recently used session is identifiable as the latest
- **Persona:** A user who wants the app to know which conversation was last touched.
- **Preconditions:** Two or more saved sessions with different last-modified times.
- **Steps:**
  1. Complete a turn in session X, then complete a later turn in session Y.
  2. Reload the page and open the sessions list.
- **Assertions:**
  - A1: Session Y (the most recently modified) is reported as the latest — it sits at the top of the newest-first list.
  - A2: With no sessions saved at all, there is no "latest" session and the list is empty (no crash, no phantom entry).
- **Traces:** matrix-132; M7

### WUI-STO-14: Renaming a session updates the list entry AND the open conversation consistently
- **Persona:** A user who renames a chat and expects the new name everywhere immediately.
- **Preconditions:** A saved session currently open with an auto-generated title.
- **Steps:**
  1. Use the header inline-title control to rename the current session to "Tax notes 2026".
  2. Open the sessions list.
  3. Reload the page, reopen the list, and load the session.
- **Assertions:**
  - A1: The header title updates to "Tax notes 2026".
  - A2: The sessions list shows "Tax notes 2026" for that entry (metadata updated).
  - A3: After reload + load, the loaded conversation's title is also "Tax notes 2026" (full record updated).
  - A4: There is never a window where the list shows one title and the opened conversation shows a different one — both records carry the new title together.
- **Traces:** matrix-132; M7

### WUI-STO-15: Renaming a session that was never saved is a harmless no-op
- **Persona:** A user who renames a brand-new, message-less session.
- **Preconditions:** A fresh session with no completed turns (nothing persisted yet).
- **Steps:**
  1. On a new, empty session, set an inline title "Draft".
  2. Reload the page and open the sessions list.
- **Assertions:**
  - A1: No error toast or console error appears from the rename.
  - A2: No phantom session named "Draft" appears in the list after reload (rename of a non-existent record changed nothing).
- **Traces:** matrix-132; M7

### WUI-STO-16: Deleting a session removes it from the list AND makes it unopenable
- **Persona:** A user cleaning up old conversations.
- **Preconditions:** At least two saved sessions; delete is reachable from the sessions list with an in-UI confirmation.
- **Steps:**
  1. Open the sessions list.
  2. Delete one session via the confirmation control.
  3. Reload the page and reopen the list.
- **Assertions:**
  - A1: The deleted session disappears from the list immediately.
  - A2: After reload, it is still gone (no resurrection).
  - A3: Its full record is also gone — there is no way to reopen the deleted conversation.
  - A4: The remaining session(s) are untouched and still openable.
- **Traces:** matrix-131; M7

### WUI-STO-17: Deleting the currently-open session starts a fresh one
- **Persona:** A user who deletes the very chat they are looking at.
- **Preconditions:** The active session is also saved and shown in the list.
- **Steps:**
  1. With session Z open, open the sessions list and delete Z.
  2. Observe the main view.
- **Assertions:**
  - A1: The app transitions to a brand-new empty session (header shows the new-session label).
  - A2: The deleted session does not reappear in the list after a reload.
  - A3: Completing a turn now creates a new record, not a revival of the deleted one.
- **Traces:** matrix-131; matrix-136; M7

### WUI-STO-18: "New session" starts a separate record without overwriting the previous one
- **Persona:** A user starting an unrelated conversation.
- **Preconditions:** One saved session with completed turns.
- **Steps:**
  1. With an existing saved conversation, click "New session".
  2. Send a message and complete a turn in the new session.
  3. Reload and open the sessions list.
- **Assertions:**
  - A1: The previous conversation is preserved unchanged.
  - A2: The new conversation appears as a distinct, separate entry.
  - A3: Both sessions are independently openable with their own transcripts.
- **Traces:** matrix-136; matrix-131; M7

### WUI-STO-19: A custom provider can be created, persists, and reloads
- **Persona:** A power user who adds a self-hosted LLM endpoint.
- **Preconditions:** Settings > Providers & Models reachable; ability to add a custom provider.
- **Steps:**
  1. Add a custom provider named "My Local LLM" with a base URL.
  2. Save it.
  3. Reload the page and reopen Providers & Models.
- **Assertions:**
  - A1: "My Local LLM" appears in the providers list immediately after saving.
  - A2: After reload, it is still listed with the same name and base URL (persisted, UUID-keyed).
  - A3: Adding a second custom provider does not overwrite the first — both coexist.
- **Traces:** matrix-133; M7

### WUI-STO-20: Editing and deleting a custom provider persists correctly
- **Persona:** A user maintaining their custom endpoints.
- **Preconditions:** At least one saved custom provider.
- **Steps:**
  1. Edit an existing custom provider's base URL and save.
  2. Reload and confirm the change.
  3. Delete that custom provider.
  4. Reload again.
- **Assertions:**
  - A1: The edited base URL persists across reload (same record updated in place, not duplicated).
  - A2: After deletion + reload, the provider is gone from the list.
  - A3: Other custom providers remain present and unchanged.
- **Traces:** matrix-133; M7

### WUI-STO-21: Sessions list scales and stays correctly ordered with many sessions
- **Persona:** A heavy user with a long history of conversations.
- **Preconditions:** Ten or more saved sessions created over time.
- **Steps:**
  1. Accumulate many sessions across multiple turns.
  2. Reload the page and open the sessions list.
  3. Touch one of the oldest sessions (complete a turn in it).
- **Assertions:**
  - A1: All saved sessions appear in the list (none silently dropped).
  - A2: They render in newest-first order without obvious lag in opening the list (metadata-only listing, full transcripts not loaded just to show the list).
  - A3: The just-touched old session jumps to the top after its turn completes.
- **Traces:** matrix-132; matrix-126; M7

### WUI-STO-22: Storage usage info shows sane values (or graceful zeros on unsupported browsers)
- **Persona:** A user curious how much space their chats consume.
- **Preconditions:** Several saved sessions; a storage-usage surface (e.g. quota indicator in settings/persistence dialog).
- **Steps:**
  1. Open the surface that reports storage usage/quota.
  2. Note the values; if available, repeat in a browser lacking the StorageManager API.
- **Assertions:**
  - A1: On a supporting browser, usage and quota are non-negative numbers and the used-percentage is between 0 and 100.
  - A2: The reported percentage is consistent with usage relative to quota (it grows after many large sessions are saved).
  - A3: On a browser without the StorageManager API, usage/quota/percent all read as zero and the UI does not error or break.
- **Traces:** matrix-126; M7

### WUI-STO-23: Requesting persistent storage succeeds or degrades gracefully
- **Persona:** A user who wants the browser not to evict their chats under storage pressure.
- **Preconditions:** A control that requests persistent storage (e.g. a persistent-storage dialog).
- **Steps:**
  1. Trigger the "make storage persistent" request.
  2. Observe the result; repeat in a browser/context that does not support persistence.
- **Assertions:**
  - A1: Where supported and granted, the UI reflects a "persistent/granted" outcome.
  - A2: Where unsupported or denied, the UI reflects a "not granted" outcome without throwing an error.
  - A3: The chat remains fully usable regardless of the persistence outcome.
- **Traces:** matrix-126; M7

### WUI-STO-24: Missing or absent records degrade gracefully (no crash)
- **Persona:** A user who bookmarked or shares a link referencing a session that no longer exists.
- **Preconditions:** A session id that is not present in storage (e.g. previously deleted).
- **Steps:**
  1. Attempt to load a session that has been deleted (e.g. trigger a load for a now-absent id).
  2. Read a setting that was never set (e.g. theme before any choice).
- **Assertions:**
  - A1: Loading a non-existent session does nothing harmful — the current view is preserved and no crash occurs.
  - A2: Reading an unset setting returns nothing and the UI falls back to its default (e.g. dark theme) without error.
  - A3: No uncaught exception or error toast results from the missing data.
- **Traces:** matrix-136; matrix-129; M7

### WUI-STO-25: Corrupt persisted data does not crash the app on load
- **Persona:** A user whose local storage got into a bad state (e.g. partial write from a prior crash).
- **Preconditions:** Ability to tamper with a stored record via DevTools (simulating corruption).
- **Steps:**
  1. In DevTools, corrupt or partially blank a `sessions` or `sessions-metadata` record.
  2. Reload the page.
  3. Open the sessions list and attempt to load the affected session, then start a new session.
- **Assertions:**
  - A1: The app still boots and the chat UI is usable.
  - A2: Listing/loading the affected session does not crash the page — at worst that one entry is skipped or fails its single load with a console warning.
  - A3: Other, healthy sessions remain listable and openable.
  - A4: Starting a new session and chatting works normally.
- **Traces:** matrix-131; matrix-136; M7

### WUI-STO-26: An IndexedDB write failure is swallowed and never crashes the chat
- **Persona:** A user whose browser blocks or fails storage (private mode, quota exceeded, disabled IndexedDB).
- **Preconditions:** A way to force storage writes to fail (e.g. fill quota, deny storage, or block IndexedDB).
- **Steps:**
  1. Put the browser into a state where IndexedDB writes fail.
  2. Send a message and let the assistant respond (a turn completes, triggering auto-save).
  3. Continue chatting with more turns.
- **Assertions:**
  - A1: The conversation continues to function — messages send, responses stream, nothing in the chat throws.
  - A2: The auto-save failure surfaces only as a non-fatal console warning (e.g. "session auto-save failed"), not a crash or blocking modal.
  - A3: The UI does not get stuck or lose the on-screen transcript because storage failed.
  - A4: If storage later recovers, a subsequent completed turn auto-saves successfully.
- **Traces:** matrix-136; matrix-131; M7

### WUI-STO-27: Successive turns overwrite the same session record, not pile up duplicates
- **Persona:** A user having a long back-and-forth in one conversation.
- **Preconditions:** A single active session.
- **Steps:**
  1. In one session, send and complete five separate turns.
  2. Reload the page and open the sessions list.
  3. Load the session.
- **Assertions:**
  - A1: The sessions list shows exactly ONE entry for that conversation, not five.
  - A2: The loaded transcript contains all the messages from all five turns, in order.
  - A3: The entry's last-modified reflects the most recent turn.
  - A4: The message count on the list entry matches the restored transcript length.
- **Traces:** matrix-136; matrix-131; M7

### WUI-STO-28: Data survives closing the tab and a later cold open (true persistence)
- **Persona:** A user who closes the browser entirely and comes back the next day.
- **Preconditions:** A completed conversation with a custom title, a chosen theme, and a stored provider key.
- **Steps:**
  1. Complete a conversation, rename it, switch theme, and store a provider key.
  2. Close the tab (and, if possible, the browser) entirely — not just reload.
  3. Reopen the app at the same URL.
- **Assertions:**
  - A1: The renamed conversation is present in the sessions list and reopens with its full transcript.
  - A2: The chosen theme is applied on cold open.
  - A3: The provider key still reads as "stored" (without revealing its value), so no re-prompt occurs.
  - A4: No data created in the prior session is lost across the cold open.
- **Traces:** matrix-136; matrix-129; matrix-130; M7
