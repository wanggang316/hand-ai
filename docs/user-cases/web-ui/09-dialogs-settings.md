# Dialogs, Settings & App Header — Web UI Acceptance Test Cases

Scope: end-user acceptance cases for the web UI's modal dialog system and top app header (M9, matrix rows 155-163): the app header (Sessions / New-session / inline-editable session title / theme toggle / Settings), the `DialogBase` modal primitive (centered panel, backdrop-click and Escape close, configurable width/height), the tabbed settings dialog (desktop sidebar / mobile horizontal strip, all tabs mounted with visibility toggled), the Proxy tab (document-fetch proxy — explicitly NOT an LLM proxy), the API Keys tab (per-provider key entry), the session-list dialog (metadata cards, relative dates, usage, in-UI two-step delete confirmation), the API-key-prompt dialog (resolve on key entry / cancel), and the persistent-storage dialog (navigator.storage.persist with graceful fallback). All confirmations and prompts are rendered in-UI; the feature uses NO `window.confirm`, `window.alert`, or `window.prompt` anywhere.

Each case is observable from the running UI by an end user. Source under test: `crates/web-ui/web/src/dialogs/*`, `crates/web-ui/web/src/shell/app-header.ts`, `crates/web-ui/web/src/ui/dialog-base.ts`.

---

### WUI-DLG-01: App header renders its five primary actions
- **Persona:** First-time user who has just opened the web UI.
- **Preconditions:** App loaded; a session is active (the title bar shows a session name, defaulting to "New Session").
- **Steps:**
  1. Look at the top bar above the chat panel.
  2. Identify each control from left to right.
- **Assertions:**
  - A1: The header is a single horizontal bar pinned to the top of the chat area (fixed height, bottom border).
  - A2: A Sessions button (message-squares icon) appears at the far left with the accessible title "Sessions".
  - A3: A New-session button (plus icon) appears immediately to its right with title "New Session".
  - A4: A centered session-title element shows the current title text.
  - A5: A theme-toggle button and a Settings button (gear icon, title "Settings") appear at the far right — five interactive actions in total.
- **Traces:** matrix row 163; M9; `app-header.ts`.

### WUI-DLG-02: Sessions button opens the session-list dialog
- **Persona:** Returning user wanting to revisit a past conversation.
- **Preconditions:** App loaded; at least the header is visible.
- **Steps:**
  1. Click the Sessions button in the header.
- **Assertions:**
  - A1: A modal dialog opens with the heading "Sessions" and the subtitle "Load a previous conversation".
  - A2: The dialog is centered over a dimmed (semi-transparent black) full-screen backdrop.
  - A3: The rest of the page behind the backdrop is not interactive while the dialog is open.
- **Traces:** matrix rows 159, 163; M9; `app-header.ts`, `session-list-dialog.ts`.

### WUI-DLG-03: New-session button resets the conversation
- **Persona:** User who has an in-progress conversation and wants a fresh start.
- **Preconditions:** App loaded with a non-empty conversation; header title shows a custom or default name.
- **Steps:**
  1. Note the current messages and title.
  2. Click the New-session button (plus icon).
- **Assertions:**
  - A1: The conversation transcript is cleared / reset to an empty new session.
  - A2: The header title returns to the default "New Session".
  - A3: No modal dialog is shown; the reset is immediate.
- **Traces:** matrix row 163; M9; `app-header.ts` (`onNewSession`).

### WUI-DLG-04: Settings button opens the settings dialog
- **Persona:** User configuring providers or the proxy.
- **Preconditions:** App loaded; header visible.
- **Steps:**
  1. Click the Settings (gear) button in the header.
- **Assertions:**
  - A1: A modal dialog opens with the heading "Settings".
  - A2: The dialog shows tab navigation including a Providers & Models tab and a Proxy tab (and an API Keys tab where wired).
  - A3: The dialog is centered over the dimmed backdrop.
- **Traces:** matrix rows 158, 162, 163; M9; `app-header.ts`, `settings-dialog.ts`.

### WUI-DLG-05: Theme toggle flips light/dark and updates its icon
- **Persona:** User who prefers a light interface.
- **Preconditions:** App loaded in the default dark theme.
- **Steps:**
  1. Observe the theme toggle button (sun icon while dark; its title reads "Switch to light theme").
  2. Click the theme toggle button.
- **Assertions:**
  - A1: The document root's `data-theme` attribute changes from `dark` to `light` and the page colors update accordingly.
  - A2: The toggle button's icon switches to a moon and its title becomes "Switch to dark theme".
  - A3: Clicking again returns to `data-theme="dark"` with the sun icon and "Switch to light theme" title.
- **Traces:** matrix row 163; M9; M11 (theming); `app-header.ts`.

### WUI-DLG-06: Theme choice persists across reload
- **Persona:** User who set light mode and reopens the app later.
- **Preconditions:** App loaded; theme toggled to light in this session.
- **Steps:**
  1. Toggle the theme to light.
  2. Reload the page.
- **Assertions:**
  - A1: After reload the document root still carries `data-theme="light"`.
  - A2: The toggle button shows the moon icon (offering to switch back to dark), reflecting the persisted choice.
- **Traces:** matrix row 163; M9/M11; `app-header.ts` (`onThemeChange` → SettingsStore).

### WUI-DLG-07: Inline session-title rename via click-to-edit
- **Persona:** User who wants a memorable name for the current chat.
- **Preconditions:** App loaded; header shows a title (e.g. "New Session").
- **Steps:**
  1. Click the centered title text in the header.
  2. Observe it becomes an editable text input pre-filled with the current title and focused with the text selected.
  3. Type a new title, e.g. "Trip planning".
  4. Press Enter.
- **Assertions:**
  - A1: Clicking the title swaps the static text for a focused `<input>` whose value equals the prior title and whose text is selected.
  - A2: After Enter the input collapses back to static text reading "Trip planning".
  - A3: The new title is persisted (it survives reload / appears as the session's title in the session list).
- **Traces:** matrix row 163; M9; `app-header.ts` (`onRenameTitle`).

### WUI-DLG-08: Title rename commits on blur
- **Persona:** User who edits the title then clicks elsewhere.
- **Preconditions:** App loaded; title edit started per WUI-DLG-07.
- **Steps:**
  1. Click the title to enter edit mode.
  2. Replace the text with "Budget review".
  3. Click outside the input (blur) without pressing Enter.
- **Assertions:**
  - A1: The input collapses to static text showing "Budget review".
  - A2: The renamed title is persisted just as with the Enter path.
- **Traces:** matrix row 163; M9; `app-header.ts` (`@blur` → `commitEdit`).

### WUI-DLG-09: Title rename canceled with Escape, no change
- **Persona:** User who starts renaming then changes their mind.
- **Preconditions:** App loaded; current title is "New Session".
- **Steps:**
  1. Click the title to enter edit mode.
  2. Type "Throwaway".
  3. Press Escape.
- **Assertions:**
  - A1: The input collapses back to static text.
  - A2: The displayed title is unchanged — still "New Session".
  - A3: No rename is persisted (the prior title remains in storage).
- **Traces:** matrix row 163; M9; `app-header.ts` (`cancelEdit`).

### WUI-DLG-10: Empty/whitespace title is rejected, original kept
- **Persona:** User who accidentally clears the title field.
- **Preconditions:** App loaded; current title "Quarterly notes".
- **Steps:**
  1. Click the title to edit.
  2. Delete all characters (or enter only spaces).
  3. Press Enter.
- **Assertions:**
  - A1: The title does not become blank; it reverts to "Quarterly notes".
  - A2: No empty rename is persisted (the trimmed-empty value is discarded).
- **Traces:** matrix row 163; M9; `app-header.ts` (`commitEdit` trims and guards empty).

### WUI-DLG-11: DialogBase opens centered with the correct panel size
- **Persona:** User opening any modal in the app.
- **Preconditions:** App loaded.
- **Steps:**
  1. Open the Settings dialog.
  2. Observe the panel geometry and position.
- **Assertions:**
  - A1: The panel is horizontally and vertically centered within the viewport.
  - A2: The settings panel honors its configured size — width `min(1000px, 90vw)` and height `min(800px, 90vh)`, capped at `max-h-[90vh]`.
  - A3: A bordered, rounded, shadowed panel sits above a dimmed backdrop covering the whole viewport.
- **Traces:** matrix row 162; M9; `dialog-base.ts`, `settings-dialog.ts`.

### WUI-DLG-12: Backdrop click closes a dialog; panel click does not
- **Persona:** User dismissing a modal by clicking outside it.
- **Preconditions:** Settings dialog (or any DialogBase dialog) is open.
- **Steps:**
  1. Click inside the dialog panel (e.g. on the heading or a tab).
  2. Click on the dimmed backdrop area outside the panel.
- **Assertions:**
  - A1: Clicking inside the panel does NOT close the dialog (the click is stopped at the panel).
  - A2: Clicking the backdrop closes the dialog and removes it from the page.
  - A3: After close the backdrop is gone and the page behind is interactive again.
- **Traces:** matrix row 162; M9; `dialog-base.ts` (backdrop target check + `stopPropagation`).

### WUI-DLG-13: Escape key closes a dialog
- **Persona:** Keyboard-oriented user.
- **Preconditions:** Any DialogBase dialog is open (e.g. Settings).
- **Steps:**
  1. Press the Escape key.
- **Assertions:**
  - A1: The dialog closes and is removed from the DOM.
  - A2: The backdrop disappears and focus returns to the page.
  - A3: An Escape pressed during IME composition (CJK input) does NOT close the dialog (composition keys are ignored).
- **Traces:** matrix row 162; M9; `dialog-base.ts` (`keydownHandler`, `isComposing`/`Process` guard).

### WUI-DLG-14: Closed dialog is fully removed and reopenable
- **Persona:** User who opens, closes, and reopens Settings.
- **Preconditions:** App loaded.
- **Steps:**
  1. Open the Settings dialog.
  2. Close it via Escape or backdrop.
  3. Open it again from the header.
- **Assertions:**
  - A1: After close, no dialog element or backdrop remains on the page.
  - A2: Reopening produces a fresh, fully-rendered dialog with the same tabs.
  - A3: No duplicate/stacked backdrops accumulate after repeated open/close.
- **Traces:** matrix row 162; M9; `dialog-base.ts` (`open`/`close` mount/unmount).

### WUI-DLG-15: Settings sidebar navigation switches tabs (desktop)
- **Persona:** Desktop user moving between settings sections.
- **Preconditions:** Settings dialog open on a wide viewport (>= md breakpoint).
- **Steps:**
  1. Observe the left sidebar listing each tab's label.
  2. Click the Proxy tab in the sidebar.
  3. Click the Providers & Models tab.
- **Assertions:**
  - A1: A vertical sidebar of tab labels is visible on the left; the active tab is highlighted (filled secondary background, stronger text weight).
  - A2: Clicking Proxy shows the Proxy tab content and highlights Proxy in the sidebar.
  - A3: Clicking back shows the Providers & Models content; exactly one tab's content is visible at a time.
- **Traces:** matrix row 158; M9; `settings-dialog.ts` (`renderSidebarItem`, `setActive`).

### WUI-DLG-16: Settings mobile strip navigation switches tabs
- **Persona:** Phone user adjusting settings.
- **Preconditions:** Settings dialog open on a narrow viewport (below md breakpoint).
- **Steps:**
  1. Observe the horizontal strip of tab buttons above the content (the desktop sidebar is hidden).
  2. Tap the Proxy tab.
- **Assertions:**
  - A1: On narrow screens the left sidebar is hidden and a horizontally scrollable tab strip is shown instead.
  - A2: The active tab in the strip is underlined / accented (bottom border in the primary color).
  - A3: Tapping a tab switches the visible content to that tab.
- **Traces:** matrix row 158; M9; `settings-dialog.ts` (`renderMobileTab`, `md:hidden`).

### WUI-DLG-17: All settings tabs stay mounted; switching only toggles visibility
- **Persona:** User who enters data in one tab, visits another, and returns.
- **Preconditions:** Settings dialog open with at least Providers & Models and Proxy tabs.
- **Steps:**
  1. Go to the Proxy tab; enable the proxy and type a custom URL into the URL field but do not navigate away yet.
  2. Switch to the Providers & Models tab.
  3. Switch back to the Proxy tab.
- **Assertions:**
  - A1: The Proxy tab's in-progress state (checkbox enabled, the URL text) is exactly as left — it was not re-initialized.
  - A2: Each tab is mounted once (its one-time load runs a single time); switching tabs toggles `display` rather than destroying/recreating tab content.
  - A3: Only the active tab is visible; inactive tabs are present but hidden (`display:none`).
- **Traces:** matrix rows 155, 158; M9; `settings-dialog.ts`, `settings-tab.ts`.

### WUI-DLG-18: Proxy tab toggles the document-fetch proxy and enables the URL field
- **Persona:** User behind CORS restrictions who needs the in-browser document fetcher to use a proxy.
- **Preconditions:** Settings dialog open on the Proxy tab; proxy currently disabled.
- **Steps:**
  1. Read the explanatory text.
  2. Toggle the "Use document-fetch proxy" checkbox on.
  3. Observe the Proxy URL input.
- **Assertions:**
  - A1: While disabled, the Proxy URL input is disabled/dimmed; enabling the checkbox makes it editable.
  - A2: The URL input shows the default placeholder `http://localhost:3001` and a format hint: the proxy must accept `<proxy-url>/?url=<target-url>`.
  - A3: The tab text makes clear this affects only in-browser document extraction and explicitly states it does NOT affect LLM calls (which are made server-side) — i.e. it is NOT an LLM proxy.
- **Traces:** matrix row 157; M9; `proxy-tab.ts`.

### WUI-DLG-19: Proxy settings persist across reload
- **Persona:** User who configured the proxy once.
- **Preconditions:** Settings dialog open on the Proxy tab.
- **Steps:**
  1. Enable the proxy checkbox.
  2. Change the Proxy URL to `http://localhost:9000` and commit it (the field saves on change).
  3. Close Settings and reload the page; reopen Settings → Proxy tab.
- **Assertions:**
  - A1: The proxy checkbox is still enabled (`proxy.enabled` persisted as true).
  - A2: The Proxy URL field still shows `http://localhost:9000` (`proxy.url` persisted).
  - A3: Toggling the checkbox off persists too — after reload it returns disabled and the URL field is non-editable.
- **Traces:** matrix row 157; M9; `proxy-tab.ts` (SettingsStore `proxy.enabled` / `proxy.url`).

### WUI-DLG-20: API Keys tab lists per-provider key inputs
- **Persona:** User entering cloud provider credentials.
- **Preconditions:** Settings dialog open with the API Keys tab available.
- **Steps:**
  1. Open the API Keys tab.
  2. Observe the list of providers and the explanatory text.
- **Assertions:**
  - A1: A short note states keys are stored locally in the browser and are configured per LLM provider.
  - A2: One key-entry row appears per provider (derived from the server's model catalog when available; otherwise a static fallback set such as anthropic, openai, google, groq, openrouter, xai).
  - A3: Each row is a per-provider key input that records the key locally and indicates key presence without revealing the stored value.
- **Traces:** matrix row 156; M9; `api-keys-tab.ts`.

### WUI-DLG-21: Session-list dialog shows metadata cards with title, relative date, count, usage
- **Persona:** Returning user scanning past conversations.
- **Preconditions:** Several saved sessions exist with varied timestamps and message counts.
- **Steps:**
  1. Open the Sessions dialog from the header.
  2. Inspect the cards.
- **Assertions:**
  - A1: Each saved session renders as a card with its title (truncated if long), a relative date, and a message count.
  - A2: Relative dates render as "Today", "Yesterday", "N days ago" (for <7 days), and an absolute locale date for older sessions.
  - A3: When usage data exists, the card appends a usage summary after the message count (e.g. "12 messages · <usage>"); when absent only the count is shown.
- **Traces:** matrix row 159; M9; `session-list-dialog.ts`, `utils/format` (`formatUsage`).

### WUI-DLG-22: Clicking a session card loads that session and closes the dialog
- **Persona:** User resuming a specific past conversation.
- **Preconditions:** Sessions dialog open with at least one card.
- **Steps:**
  1. Click anywhere on a session card (not on its delete control).
- **Assertions:**
  - A1: The selected session is loaded into the chat view (its transcript/title becomes the active session).
  - A2: The Sessions dialog closes immediately after selection.
  - A3: The header title updates to reflect the loaded session.
- **Traces:** matrix row 159; M9; `session-list-dialog.ts` (`handleSelect` → `onLoad` + `close`).

### WUI-DLG-23: Per-card delete uses an in-UI two-step confirm (no window.confirm)
- **Persona:** User cleaning up old sessions, mindful of accidental deletes.
- **Preconditions:** Sessions dialog open with at least two cards.
- **Steps:**
  1. Hover a card to reveal its trash/delete icon and click it.
  2. Observe the inline confirmation controls.
  3. Click the inline "Delete" (confirm) button.
- **Assertions:**
  - A1: Clicking the trash icon does NOT trigger any browser `window.confirm` dialog; instead the icon is replaced inline by a "Delete" (destructive-styled) button and a "Cancel" button on the same card.
  - A2: Clicking the card body while in confirm mode does NOT load the session (the confirm controls stop propagation).
  - A3: Clicking the inline "Delete" removes the session from storage and the list refreshes without that card; remaining cards stay intact.
- **Traces:** matrix row 159; M9; `session-list-dialog.ts` (`renderDeleteControl`, `confirmDelete`).

### WUI-DLG-24: Canceling the in-UI delete confirm keeps the session
- **Persona:** User who clicked delete by mistake.
- **Preconditions:** Sessions dialog open; a card showing inline Delete/Cancel confirm controls.
- **Steps:**
  1. With a card in confirm mode, click the inline "Cancel" button.
- **Assertions:**
  - A1: The card reverts to its normal state (trash icon only); no deletion occurs.
  - A2: The session remains in the list and in storage.
  - A3: No browser confirm/alert dialog was ever shown.
- **Traces:** matrix row 159; M9; `session-list-dialog.ts` (`confirmingId` reset).

### WUI-DLG-25: Empty session list shows an empty-state, not an error
- **Persona:** Brand-new user with no saved conversations.
- **Preconditions:** No sessions have been persisted yet.
- **Steps:**
  1. Open the Sessions dialog.
- **Assertions:**
  - A1: While loading the dialog briefly shows a "Loading..." indicator.
  - A2: With no sessions it shows the message "No sessions yet" (centered, muted) instead of any cards.
  - A3: No JavaScript error is surfaced and the dialog can still be closed normally.
- **Traces:** matrix row 159; M9; `session-list-dialog.ts` (loading / empty branches).

### WUI-DLG-26: API-key-prompt dialog resolves once a key is entered
- **Persona:** User who triggered an action needing a provider key that is not yet set.
- **Preconditions:** A flow invokes the API-key prompt for a provider (e.g. "openai") that has no stored key.
- **Steps:**
  1. Observe the prompt dialog.
  2. Enter a valid key into the provider key input and save it.
- **Assertions:**
  - A1: The dialog shows the heading "API Key Required" and a message naming the specific provider ("Enter an API key for openai to continue.").
  - A2: Once the key is stored, the dialog detects its presence and closes automatically.
  - A3: The originating flow continues — the prompt resolves success (true) so the gated action proceeds.
- **Traces:** matrix row 160; M9; `api-key-prompt-dialog.ts` (storage-poll resolve).

### WUI-DLG-27: API-key-prompt cancel path resolves as not-provided
- **Persona:** User who decides not to supply a key.
- **Preconditions:** The API-key prompt dialog is open for a provider with no stored key.
- **Steps:**
  1. Click the in-dialog "Cancel" button (or press Escape, or click the backdrop).
- **Assertions:**
  - A1: The dialog closes without storing a key.
  - A2: The prompt resolves as not-provided (false) so the gated action is abandoned, not silently retried.
  - A3: The background poll/timer is cleared on close (no leftover polling after cancel); no browser prompt/alert is used.
- **Traces:** matrix row 160; M9; `api-key-prompt-dialog.ts` (`close` resolves false, `clearPoll`).

### WUI-DLG-28: Persistent-storage dialog requests persistence and resolves on grant
- **Persona:** User asked to protect their locally-saved conversations.
- **Preconditions:** Storage is not yet persisted; the StorageManager API is supported by the browser.
- **Steps:**
  1. Trigger the persistent-storage request; observe the dialog.
  2. Click "Grant Permission".
- **Assertions:**
  - A1: The dialog shows the heading "Storage Permission", an explanation, and a bullet list (data saved locally, not auto-deleted, nothing sent to external servers).
  - A2: Clicking "Grant Permission" calls `navigator.storage.persist()`; the button shows "Requesting..." while in flight.
  - A3: On a granted result the dialog closes and the request resolves true.
- **Traces:** matrix row 161; M9; `persistent-storage-dialog.ts` (`grant`).

### WUI-DLG-29: Persistent-storage "Continue Anyway" resolves without persistence
- **Persona:** User who declines the persistence request.
- **Preconditions:** Persistent-storage dialog open.
- **Steps:**
  1. Click "Continue Anyway".
- **Assertions:**
  - A1: The dialog closes without calling persist.
  - A2: The request resolves false; the app keeps working with non-persistent local storage.
  - A3: No browser confirm/alert is used; the choice is entirely in-UI.
- **Traces:** matrix row 161; M9; `persistent-storage-dialog.ts` (`close` resolves false).

### WUI-DLG-30: Persistent-storage degrades gracefully when unsupported
- **Persona:** User on a browser without the StorageManager persist API.
- **Preconditions:** `navigator.storage.persist` is unavailable.
- **Steps:**
  1. Open the persistent-storage dialog.
  2. Click "Grant Permission".
- **Assertions:**
  - A1: The dialog still renders normally (it does not crash or throw).
  - A2: Instead of an error, an in-UI message appears explaining persistence is not supported and data is still saved locally but may be cleared under storage pressure.
  - A3: No `window.alert`/`window.confirm` is used; the user can still "Continue Anyway".
- **Traces:** matrix row 161; M9; `persistent-storage-dialog.ts` (graceful fallback branch).

### WUI-DLG-31: Persistent-storage skips the dialog when already persisted
- **Persona:** Returning user whose storage is already persistent.
- **Preconditions:** `navigator.storage.persisted()` returns true.
- **Steps:**
  1. Trigger the persistent-storage request.
- **Assertions:**
  - A1: No dialog is shown.
  - A2: The request resolves true immediately.
- **Traces:** matrix row 161; M9; `persistent-storage-dialog.ts` (`request` early-return).

### WUI-DLG-32: No native browser dialogs anywhere in this feature
- **Persona:** QA validator auditing for blocking native popups.
- **Preconditions:** App loaded; ability to exercise every dialog and the delete-confirm flow.
- **Steps:**
  1. Open and close the Settings, Sessions, API-key-prompt, and persistent-storage dialogs.
  2. Exercise the per-card delete confirm/cancel and the persistent-storage unsupported path.
- **Assertions:**
  - A1: At no point is a native `window.confirm`, `window.alert`, or `window.prompt` dialog shown.
  - A2: Every confirmation, prompt, and error message is rendered inside the app's own in-UI components.
  - A3: All dialogs are closeable via in-UI controls and standard Escape/backdrop interactions.
- **Traces:** matrix rows 159, 160, 161, 162; M9; `dialogs/*`, `dialog-base.ts`.
