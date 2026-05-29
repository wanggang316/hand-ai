# Providers & Models — Web UI Acceptance Test Cases

Scope: end-user acceptance criteria for provider and model management in the web UI — opening the keyboard-navigable model selector from the editor, loading the server model catalog (`get_available_models`), subsequence fuzzy search, Thinking/Vision capability filters, keyboard navigation, current-model checkmark, `allowedProviders` restriction, cost and context formatting, switching/cycling the active model, custom-provider CRUD with Test Connection and status indicators, auto-discovery for Ollama / llama.cpp / vLLM / LM Studio, default base-URL prefill, the cloud-provider API-key rows (presence-without-reveal + server round-trip validation), and the Providers & Models settings tab. Covers empty-search, no-models, and discovery-failure edge cases. Derived from M8 and Capability Parity Matrix rows 137–154.

### WUI-MDL-01: Open the model selector from the editor's model button
- **Persona:** End user choosing a model
- **Preconditions:** A chat session is open; the editor shows the model button labeled with the current model id; the server is reachable.
- **Steps:**
  1. Locate the model button at the bottom-right of the message editor.
  2. Click the button.
  3. Wait briefly for the dialog to render.
- **Assertions:**
  - A1: A modal titled "Select Model" appears centered over the chat.
  - A2: A search input with a magnifier glyph and placeholder "Search models..." is shown and receives focus automatically.
  - A3: Two pill filter buttons labeled "Thinking" and "Vision" are visible below the search field.
  - A4: The editor's textarea regains focus first, then the dialog opens (the dialog is not immediately dismissed by the same click that opened it).
- **Traces:** model-selector.ts (`open`, `firstUpdated`, `renderContent`); message-editor.ts (model button `onClick` with `setTimeout(0)`); chat-panel.ts (`openModelSelector`); matrix row 141

### WUI-MDL-02: Full model catalog loads from the server
- **Persona:** End user browsing available models
- **Preconditions:** The editor is wired to a live server agent; provider keys / model registry are configured server-side.
- **Steps:**
  1. Open the model selector from the editor.
  2. Observe the scrollable list area.
- **Assertions:**
  - A1: After the catalog request resolves, the list is populated with model rows fetched from the server's `get_available_models`.
  - A2: Each row shows a model id, a provider badge, capability glyphs (Thinking/Vision), a context figure, and a cost figure.
  - A3: Multiple distinct providers appear across the rows (the registry is not limited to one provider).
- **Traces:** model-selector.ts (`loadModels`, `renderContent`); remote-agent.ts (`getAvailableModels`); matrix row 137

### WUI-MDL-03: Catalog request failure leaves an empty list, not a crash
- **Persona:** End user when the server is unreachable
- **Preconditions:** The server connection drops or `get_available_models` errors.
- **Steps:**
  1. Open the model selector while the catalog request will fail.
  2. Wait for the request to settle.
- **Assertions:**
  - A1: The dialog still opens and stays interactive (search input focusable, filters clickable).
  - A2: The list area shows the centered empty-state text "No models found".
  - A3: No uncaught error blocks the rest of the UI; the dialog can be closed normally.
- **Traces:** model-selector.ts (`loadModels` catch → `catalogModels = []`, empty-state branch); matrix row 137

### WUI-MDL-04: Subsequence fuzzy search filters and ranks rows
- **Persona:** End user searching for a model
- **Preconditions:** The selector is open with a populated catalog including a "claude-sonnet" family model.
- **Steps:**
  1. Type `claude sonnet` into the search field.
- **Assertions:**
  - A1: The list narrows to rows whose "provider id name" text contains the typed characters in order (e.g. claude-sonnet rows).
  - A2: Rows are ordered by match tightness — the closest match appears at the top.
  - A3: Whitespace in the query is ignored (`claude sonnet` matches the same rows as `claudesonnet`).
  - A4: The selection highlight resets to the first row and the list scrolls back to the top after the query changes.
- **Traces:** model-selector.ts (`getFilteredModels` search branch, `resetScrollAndSelection`, `subsequenceScore`); matrix row 138

### WUI-MDL-05: Search with no matches shows the empty state
- **Persona:** End user mistyping a query
- **Preconditions:** The selector is open with a populated catalog.
- **Steps:**
  1. Type a string that matches no model, e.g. `zzqqxx`.
- **Assertions:**
  - A1: The list area shows the centered "No models found" message.
  - A2: Clearing the search input restores the full catalog list.
- **Traces:** model-selector.ts (`getFilteredModels` returns empty, empty-state branch); matrix row 138

### WUI-MDL-06: Thinking capability filter
- **Persona:** End user who needs a reasoning-capable model
- **Preconditions:** The selector is open; the catalog contains both reasoning and non-reasoning models.
- **Steps:**
  1. Click the "Thinking" filter pill.
- **Assertions:**
  - A1: The "Thinking" pill switches to its active (filled) style.
  - A2: Only rows for models with reasoning support remain in the list.
  - A3: Each remaining row shows the Thinking (brain) glyph at full opacity rather than dimmed.
  - A4: Clicking the pill again deactivates it and restores the unfiltered list.
- **Traces:** model-selector.ts (`filterThinking`, `getFilteredModels` reasoning filter, row glyph opacity); matrix row 139

### WUI-MDL-07: Vision capability filter
- **Persona:** End user who needs an image-capable model
- **Preconditions:** The selector is open; the catalog contains both vision and text-only models.
- **Steps:**
  1. Click the "Vision" filter pill.
- **Assertions:**
  - A1: The "Vision" pill switches to its active style.
  - A2: Only rows for models whose input includes "image" remain.
  - A3: Each remaining row shows the Vision (image) glyph at full opacity.
  - A4: The Thinking and Vision filters combine (both active leaves only models that are both reasoning and vision-capable).
- **Traces:** model-selector.ts (`filterVision`, `getFilteredModels` `input.includes("image")`); matrix row 140

### WUI-MDL-08: Keyboard navigation — arrows move, Enter selects
- **Persona:** Keyboard-first end user
- **Preconditions:** The selector is open with a populated list; focus is in the search field.
- **Steps:**
  1. Press ArrowDown twice.
  2. Press ArrowUp once.
  3. Press Enter.
- **Assertions:**
  - A1: ArrowDown moves the highlighted row down one position per press; ArrowUp moves it up one.
  - A2: The highlighted row stays visible — the list auto-scrolls to keep it in view.
  - A3: Arrow navigation does not move the cursor or scroll the page (default is prevented).
  - A4: Pressing Enter selects the highlighted model and closes the dialog.
- **Traces:** model-selector.ts (`keydown` ArrowDown/ArrowUp/Enter, `scrollToSelected`, `handleSelect`); matrix row 141

### WUI-MDL-09: Arrow navigation clamps at list bounds
- **Persona:** Keyboard-first end user
- **Preconditions:** The selector is open with a populated list; first row highlighted.
- **Steps:**
  1. Press ArrowUp repeatedly while on the first row.
  2. Press ArrowDown repeatedly until past the last row.
- **Assertions:**
  - A1: ArrowUp at the top keeps the highlight on the first row (index does not go negative).
  - A2: ArrowDown at the bottom keeps the highlight on the last row (index does not exceed the last item).
- **Traces:** model-selector.ts (`Math.max(..., 0)` / `Math.min(..., models.length - 1)`); matrix row 141

### WUI-MDL-10: Escape closes the selector without changing the model
- **Persona:** End user who decides not to switch
- **Preconditions:** The selector is open over an active session with a known current model.
- **Steps:**
  1. Press Escape (or click outside the dialog).
- **Assertions:**
  - A1: The dialog closes.
  - A2: The editor's model button still shows the original current-model id (no model change occurred).
- **Traces:** dialog-base.ts (Escape/outside-click close); model-selector.ts (`handleSelect` not invoked); matrix row 141

### WUI-MDL-11: IME composition keystrokes do not trigger navigation or selection
- **Persona:** End user typing with an IME (e.g. Chinese/Japanese input)
- **Preconditions:** The selector is open; an IME composition session is active in the search field.
- **Steps:**
  1. Begin composing text with the IME (composition in progress).
  2. Press Enter to commit the composition candidate (not to select a model).
- **Assertions:**
  - A1: Enter pressed during composition commits the IME candidate and does NOT select a model or close the dialog.
  - A2: Arrow keys during composition do not move the row highlight.
  - A3: After composition ends, the committed text is present in the search field and subsequent Enter/arrows behave normally.
- **Traces:** model-selector.ts (`keydown` guard `e.isComposing || e.key === "Process"`); matrix row 141

### WUI-MDL-12: Current model shows a checkmark and floats to the top
- **Persona:** End user confirming which model is active
- **Preconditions:** The selector is open with no search query; the session has a known current model present in the catalog.
- **Steps:**
  1. Observe the list with an empty search field.
- **Assertions:**
  - A1: The current model's row displays a green checkmark next to its id.
  - A2: With no search query, the current model is sorted to the top of the list.
  - A3: Remaining rows are ordered by provider name.
- **Traces:** model-selector.ts (`modelsAreEqual`, current-model sort branch, checkmark render); matrix row 141

### WUI-MDL-13: Mouse hover and keyboard navigation do not fight each other
- **Persona:** End user mixing mouse and keyboard
- **Preconditions:** The selector is open with a populated list; the cursor is resting over a row.
- **Steps:**
  1. Without moving the mouse, press ArrowDown.
  2. Then physically move the mouse over a different row.
- **Assertions:**
  - A1: Pressing ArrowDown moves the highlight by keyboard even though the cursor is stationary over a row (a layout-driven mouseenter does not steal selection).
  - A2: Genuinely moving the mouse over a row updates the highlight to the hovered row.
- **Traces:** model-selector.ts (`navigationMode`, mousemove position-change guard, `mouseenter` handler); matrix row 141

### WUI-MDL-14: Per-row cost formatting
- **Persona:** End user comparing model prices
- **Preconditions:** The selector is open; the catalog includes paid and free (zero-cost) models.
- **Steps:**
  1. Inspect the cost figure on the right of several rows.
- **Assertions:**
  - A1: Paid models show a `$in/$out` per-million-token figure (e.g. `$3/$15`).
  - A2: Models whose input and output rates are both zero/absent show the localized "Free" label.
  - A3: Numbers are scaled by magnitude (large values integer-rounded, small values keep significant decimals, trailing zeros trimmed).
- **Traces:** model-selector.ts (cost cell); format.ts (`formatModelCost`); matrix row 143

### WUI-MDL-15: Per-row context/output sizing formatting
- **Persona:** End user comparing context windows
- **Preconditions:** The selector is open; the catalog includes models with K-scale and M-scale context windows.
- **Steps:**
  1. Inspect the sizing figure (left group) on several rows.
- **Assertions:**
  - A1: Each row shows context/output as `<context>K/<maxTokens>K`.
  - A2: Sub-million windows render as a bare thousands figure (e.g. 200000 → `200`).
  - A3: Million-scale windows render with an `M` suffix (e.g. 1000000 → `1M`).
- **Traces:** model-selector.ts (`formatTokens(model.contextWindow)`+`K/`...); format.ts (`formatTokens`); matrix row 143

### WUI-MDL-16: Selecting a model switches it server-side and updates the editor label
- **Persona:** End user switching the active model
- **Preconditions:** The selector is open over a live session; a different model than the current one is visible.
- **Steps:**
  1. Click a model row different from the current model (or highlight it and press Enter).
- **Assertions:**
  - A1: The dialog closes.
  - A2: The editor's model button label updates to the newly chosen model id.
  - A3: A `set_model` request carrying the chosen provider and model id is sent to the server.
  - A4: Reopening the selector shows the checkmark on the newly chosen model.
- **Traces:** model-selector.ts (`handleSelect` → `onSelectCallback`); chat-panel.ts (`agent.setModel`); remote-agent.ts (`setModel` → `set_model`); agent-interface.ts (`.currentModel=${state.model}`); matrix row 141

### WUI-MDL-17: allowedProviders restricts the candidate set
- **Persona:** End user in a context limited to specific providers
- **Preconditions:** The selector is opened with an `allowedProviders` list (e.g. only `anthropic`).
- **Steps:**
  1. Open the selector in that restricted context.
  2. Browse and search the list.
- **Assertions:**
  - A1: Only rows whose provider is in the allowed set appear.
  - A2: Searching never surfaces a model from a non-allowed provider.
  - A3: With no allowed provider matching any catalog model, the list shows "No models found".
- **Traces:** model-selector.ts (`allowedProviders` filter in `getFilteredModels`); matrix row 142

### WUI-MDL-18: Custom-provider models merge into the selector list
- **Persona:** End user who has configured a local custom provider
- **Preconditions:** A reachable auto-discovery custom provider (e.g. Ollama) and/or a manual provider with persisted models exist in storage.
- **Steps:**
  1. Open the model selector.
  2. Wait for the catalog and custom-provider models to load.
- **Assertions:**
  - A1: Models discovered from the custom provider appear in the list alongside cloud models.
  - A2: Each custom-provider row's provider badge shows the user-given provider name.
  - A3: A failing custom provider does not block the rest of the list — cloud and reachable-custom models still render.
- **Traces:** model-selector.ts (`loadCustomProviders`, merge in `getFilteredModels`, per-provider try/catch); matrix rows 137/144

### WUI-MDL-19: Cycle to the next model from the keyboard
- **Persona:** End user quickly rotating among models
- **Preconditions:** A live session with a model set; the server supports model cycling.
- **Steps:**
  1. Trigger the cycle-model action.
- **Assertions:**
  - A1: A `cycle_model` command is sent to the server.
  - A2: After the server responds, the editor's model label reflects the new active model.
- **Traces:** wire.ts (`CycleModelCommand` `cycle_model`); agent-interface.ts (`.currentModel=${state.model}`); matrix row 141

### WUI-MDL-20: Open the Providers & Models settings tab
- **Persona:** End user managing providers
- **Preconditions:** The settings dialog is available from the app header.
- **Steps:**
  1. Open settings and select the "Providers & Models" tab.
- **Assertions:**
  - A1: A "Cloud Providers" section lists one API-key row per known cloud provider with explanatory text that keys are stored locally in the browser.
  - A2: A "Custom Providers" section follows, with an "Add Provider" dropdown.
  - A3: The two sections are separated by a divider.
- **Traces:** providers-models-tab.ts (`renderContent`, `renderCloud`, `renderCustom`); matrix row 154

### WUI-MDL-21: Cloud-provider list is derived from the server catalog
- **Persona:** End user verifying supported providers
- **Preconditions:** The settings tab is wired to a live agent whose catalog contains several providers.
- **Steps:**
  1. Open the Providers & Models tab.
  2. Observe the Cloud Providers rows after the catalog loads.
- **Assertions:**
  - A1: The cloud-provider rows match the distinct provider names returned by the server catalog, sorted alphabetically.
  - A2: When no agent/catalog is available, a static fallback provider list is shown instead (the section is never empty).
- **Traces:** providers-models-tab.ts (`loadCloudProviders`, `FALLBACK_CLOUD_PROVIDERS`); matrix row 154

### WUI-MDL-22: Stored API key shows a checkmark without revealing the value
- **Persona:** End user who already saved a provider key
- **Preconditions:** A key is stored for a given cloud provider.
- **Steps:**
  1. Open the Providers & Models tab.
  2. Inspect that provider's row.
- **Assertions:**
  - A1: A green checkmark with a "Key stored" title appears next to the provider name.
  - A2: The key input is a password field whose placeholder shows masking dots, never the actual stored secret.
  - A3: A "Remove" button is offered only when a key is stored.
- **Traces:** provider-key-input.ts (`refreshKeyStatus`, `hasKey` checkmark, masked placeholder, conditional Remove); matrix row 152

### WUI-MDL-23: Saving a new API key validates via a server round-trip
- **Persona:** End user entering a fresh provider key
- **Preconditions:** The Providers & Models tab is open and wired to a live agent; a provider row has no key yet.
- **Steps:**
  1. Type a key into the provider's password input.
  2. Click "Save".
- **Assertions:**
  - A1: While saving, a transient "Saving..." indicator appears and the Save button is disabled.
  - A2: On success the key is persisted locally, the input clears, and the green stored-key checkmark appears.
  - A3: A catalog refresh (`getAvailableModels`) is issued after the save so the new key is exercised server-side.
  - A4: A failed local save shows a transient "Failed to save" message that auto-dismisses; a failed catalog refresh does NOT revert the saved key.
- **Traces:** provider-key-input.ts (`saveKey`, `agent.getAvailableModels`, `failed` banner); matrix row 153

### WUI-MDL-24: Remove a stored API key
- **Persona:** End user rotating out a key
- **Preconditions:** A provider row shows a stored-key checkmark.
- **Steps:**
  1. Click "Remove" on that provider row.
- **Assertions:**
  - A1: The stored-key checkmark disappears.
  - A2: The placeholder reverts to "Enter API key" and the Remove button is no longer shown.
- **Traces:** provider-key-input.ts (`clearKey`, conditional render); matrix row 152

### WUI-MDL-25: Add a custom provider with default base-URL prefill
- **Persona:** End user adding a local model server
- **Preconditions:** The Providers & Models tab is open.
- **Steps:**
  1. Pick "Ollama" from the Add Provider dropdown.
  2. Observe the opened dialog's fields.
- **Assertions:**
  - A1: An "Add Provider" dialog opens with Name, Type, Base URL, and optional API Key fields.
  - A2: The Base URL is prefilled with the Ollama default `http://localhost:11434`.
  - A3: Switching the Type to llama.cpp / vLLM / LM Studio re-prefills the matching default (`:8080` / `:8000` / `:1234`) and clears any prior discovered models.
  - A4: The Add Provider dropdown resets to its placeholder after a type is picked.
- **Traces:** custom-provider-dialog.ts (`prefillBaseUrl`, type-change handler); discovery.ts (`DEFAULT_BASE_URLS`); providers-models-tab.ts (dropdown reset); matrix rows 144/151

### WUI-MDL-26: Test Connection lists the first 5 discovered models
- **Persona:** End user verifying a local server before saving
- **Preconditions:** The Add/Edit dialog is open for an auto-discovery type with a reachable base URL.
- **Steps:**
  1. Click "Test Connection".
- **Assertions:**
  - A1: The button shows "Testing..." while the probe runs and is disabled when the Base URL is empty.
  - A2: On success a "Discovered N models" summary appears listing up to the first 5 model names.
  - A3: When more than 5 models are discovered, a trailing "...and N more" line is shown.
- **Traces:** custom-provider-dialog.ts (`testConnection`, discovered-models render, `slice(0, 5)`); matrix row 149

### WUI-MDL-27: Test Connection failure shows an error and no model list
- **Persona:** End user pointing at an unreachable/wrong server
- **Preconditions:** The Add/Edit dialog is open for an auto-discovery type; the base URL is wrong or the server is down.
- **Steps:**
  1. Click "Test Connection".
- **Assertions:**
  - A1: A destructive-styled error message describing the failure is shown.
  - A2: No discovered-models list is rendered.
  - A3: The dialog stays open so the user can correct the URL and retry.
- **Traces:** custom-provider-dialog.ts (`testConnection` catch → `testError`); discovery.ts (HTTP-error throws); matrix rows 149/150

### WUI-MDL-28: Save validation requires name and base URL
- **Persona:** End user submitting an incomplete custom provider
- **Preconditions:** The Add Provider dialog is open with an empty Name and/or Base URL.
- **Steps:**
  1. Click "Save" with a required field blank.
- **Assertions:**
  - A1: The Save button is disabled while Name or Base URL is empty.
  - A2: If save is attempted with a missing field, a "Please fill in all required fields" message is shown and nothing is persisted.
- **Traces:** custom-provider-dialog.ts (`save` guard, Save `disabled`); matrix row 144

### WUI-MDL-29: Persist a custom provider (UUID-keyed) and see it as a card
- **Persona:** End user saving a new provider
- **Preconditions:** The Add Provider dialog is filled with a valid Name and Base URL.
- **Steps:**
  1. Click "Save".
  2. Return to the Providers & Models tab.
- **Assertions:**
  - A1: The dialog closes and a new custom-provider card appears with the given name, type (capitalized), and base URL.
  - A2: The provider is stored under a generated UUID and survives a reload of the tab.
  - A3: For a manual (non-auto-discovery) type, the card shows a "Models: 0" count.
- **Traces:** custom-provider-dialog.ts (`save`, `crypto.randomUUID()`, store `set`); providers-models-tab.ts (`loadCustomProviders`); custom-provider-card.ts (manual `renderStatus`); matrix row 144

### WUI-MDL-30: Auto-discovery provider card shows a live status indicator
- **Persona:** End user checking whether a local server is reachable
- **Preconditions:** At least one auto-discovery custom provider is configured.
- **Steps:**
  1. Open the Providers & Models tab.
  2. Watch the auto-discovery card's status line.
- **Assertions:**
  - A1: While probing, a yellow dot with "Checking..." is shown.
  - A2: On a reachable server, a green dot with "<N> models" (the discovered count) is shown.
  - A3: On an unreachable server, a red dot with "Disconnected" is shown.
  - A4: Clicking "Refresh" re-runs the probe and updates the indicator.
- **Traces:** providers-models-tab.ts (`probeProvider`, `loadCustomProviders` auto-probe); custom-provider-card.ts (`renderStatus` dot/text, Refresh button); matrix row 150

### WUI-MDL-31: Edit an existing custom provider
- **Persona:** End user correcting a provider's settings
- **Preconditions:** A custom provider card is visible.
- **Steps:**
  1. Click "Edit" on the card.
  2. Change the Base URL and Save.
- **Assertions:**
  - A1: The dialog title reads "Edit Provider" and the fields are prefilled with the existing name/type/base URL/key.
  - A2: Saving keeps the same provider id (no duplicate card is created).
  - A3: The card reflects the updated base URL after save.
- **Traces:** custom-provider-dialog.ts (`initializeFromProvider`, `save` reuses `editing.id`); providers-models-tab.ts (`editProvider`); matrix row 144

### WUI-MDL-32: Delete a custom provider
- **Persona:** End user removing a provider
- **Preconditions:** A custom provider card is visible.
- **Steps:**
  1. Click "Delete" on the card.
- **Assertions:**
  - A1: The card is removed from the Custom Providers list.
  - A2: Its cached status indicator is discarded.
  - A3: When the last custom provider is deleted, the empty-state text "No custom providers configured. Click 'Add Provider' to get started." is shown.
- **Traces:** providers-models-tab.ts (`deleteProvider`, status `delete`, empty-state branch); matrix row 144

### WUI-MDL-33: Ollama discovery surfaces only tool-capable models with real context length
- **Persona:** End user with an Ollama server
- **Preconditions:** The Ollama server has a mix of tool-capable and non-tool models.
- **Steps:**
  1. Add an Ollama provider and run Test Connection (or Refresh its card).
- **Assertions:**
  - A1: Models that do not advertise the `tools` capability are excluded from the discovered list.
  - A2: A tool-capable model's context window reflects the server-reported architecture `context_length` (falling back to 8192 when absent).
  - A3: A model that advertises `thinking` is marked reasoning-capable in the selector.
- **Traces:** discovery.ts (`discoverOllamaModels` `/api/tags`+`/api/show`, tools filter, `context_length`, thinking→reasoning); matrix row 145

### WUI-MDL-34: llama.cpp and vLLM discovery via /v1/models
- **Persona:** End user with a llama.cpp or vLLM server
- **Preconditions:** The server exposes an OpenAI-compatible `/v1/models` list.
- **Steps:**
  1. Add a llama.cpp (or vLLM) provider and run Test Connection.
- **Assertions:**
  - A1: The discovered model ids match the `/v1/models` `data[].id` entries.
  - A2: For vLLM, each model's context window reflects the reported `max_model_len` (8192 fallback) with output capped at min(context, 4096).
  - A3: A non-array or error `/v1/models` response yields a clear discovery-failed error rather than a partial list.
- **Traces:** discovery.ts (`discoverLlamaCppModels`, `discoverVLLMModels`, `fetchOpenAiModels` validation); matrix rows 146/147

### WUI-MDL-35: LM Studio discovery prefers the REST API with capability hints
- **Persona:** End user with an LM Studio server
- **Preconditions:** LM Studio is running with its REST API available.
- **Steps:**
  1. Add an LM Studio provider and run Test Connection.
- **Assertions:**
  - A1: Discovered entries come from the LM Studio REST `/api/v0/models` list, restricted to LLM-type models.
  - A2: Each model reflects its reported context length and vision capability hint.
  - A3: If the REST API is unavailable, discovery falls back to the OpenAI-compatible `/v1/models` list rather than failing outright.
- **Traces:** discovery.ts (`discoverLMStudioModels` REST-then-`/v1/models` fallback, type/vision/context); matrix row 148

### WUI-MDL-36: Discovery failure is non-fatal to the selector
- **Persona:** End user opening the selector while a local server is down
- **Preconditions:** A custom auto-discovery provider is configured but its server is unreachable; the cloud catalog is reachable.
- **Steps:**
  1. Open the model selector.
- **Assertions:**
  - A1: The selector still renders the reachable cloud (and other reachable custom) models.
  - A2: No models from the failed provider appear, and the failure does not produce an empty or broken list.
  - A3: The unreachable provider's settings card independently shows the red "Disconnected" status.
- **Traces:** model-selector.ts (`loadCustomProviders` per-provider try/catch → `finally`); providers-models-tab.ts (`probeProvider` catch → disconnected); discovery.ts; matrix rows 145–150
