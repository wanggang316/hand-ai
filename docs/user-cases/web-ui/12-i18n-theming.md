# i18n, Theming & Design System — Web UI Acceptance Test Cases

Scope: end-user behavior of the web UI presentation layer — runtime translation (`i18n()` lookup, `{param}` substitution, English↔German switching with live re-render), token/cost formatting (`formatUsage`, `formatCost`, `formatModelCost`, `formatTokenCount`), light/dark theming (the header toggle, `data-theme`, settings persistence, OS `prefers-color-scheme` honoring), design tokens resolving in both themes, the thinking-block shimmer, thin scrollbars, the user-message gradient, and the reusable UI primitives (Button incl. CopyButton/DownloadButton, Input, Select, Switch, Badge, Label). Every visible string must resolve through the translation layer (no hardcoded brand text).

### WUI-UIX-01: Active-language lookup returns the translation
- **Persona:** German-speaking user with the UI language set to German
- **Preconditions:** The application is loaded and the active language is German
- **Steps:**
  1. Open any surface that shows a Cancel control (e.g. a confirmation dialog).
  2. Open the settings dialog and observe its title.
- **Assertions:**
  - A1: The Cancel control reads "Abbrechen".
  - A2: The settings dialog title reads "Einstellungen".
  - A3: A close affordance reads "Schließen".
- **Traces:** rows 171, 172; M11

### WUI-UIX-02: Missing key falls back to the key itself (English source)
- **Persona:** User on a screen whose label has no German override
- **Preconditions:** The active language is German; a visible label exists whose key is not present in the German table (e.g. "Thinking" or "Vision", which are intentionally left identical)
- **Steps:**
  1. Open the model selector so model capability tags are visible.
  2. Observe a label whose key has no distinct German translation.
- **Assertions:**
  - A1: The "Thinking" capability tag still renders as "Thinking" (no blank, no raw `{}` artifacts).
  - A2: The "Vision" capability tag still renders as "Vision".
  - A3: No label ever renders as an empty string when its key is missing from the table.
- **Traces:** rows 171, 172; M11

### WUI-UIX-03: setLanguage("de") switches every visible UI string to German and re-renders live
- **Persona:** User changing the interface language without reloading
- **Preconditions:** The app is loaded in English with the header, editor placeholder, and Settings button visible
- **Steps:**
  1. Note the current English strings: the message editor placeholder, the Send action, the Settings button title, the Sessions button title.
  2. Switch the active language to German (`setLanguage("de")`).
  3. Without reloading, observe the same elements.
- **Assertions:**
  - A1: The editor placeholder changes from "Type a message..." to "Nachricht eingeben...".
  - A2: The Send affordance changes from "Send" to "Senden".
  - A3: The Settings button title changes from "Settings" to "Einstellungen"; the Sessions title changes to "Sitzungen".
  - A4: The change is applied in place (a re-render is triggered by the language change), with no full page reload required.
- **Traces:** rows 171, 172; M11

### WUI-UIX-04: language-change notification re-renders long-lived components
- **Persona:** User watching the header (a persistent custom element) during a language switch
- **Preconditions:** The app is loaded; the header (with its theme-toggle title and other button titles) is mounted
- **Steps:**
  1. With the language in English, hover the theme toggle and read its tooltip.
  2. Switch the active language to German.
  3. Re-hover the theme toggle without reloading.
- **Assertions:**
  - A1: Before the switch, the toggle tooltip reads "Switch to dark theme" or "Switch to light theme" (English).
  - A2: After the switch, the same toggle's tooltip reads "Zum dunklen Design wechseln" or "Zum hellen Design wechseln" (German).
  - A3: The persistent header element updates its visible strings in response to the language-change notification, not only freshly created elements.
- **Traces:** rows 171, 172; M11

### WUI-UIX-05: {param} placeholder substitution — "{days} days ago" with days=3
- **Persona:** User reviewing the session list with relative timestamps
- **Preconditions:** A saved session was last modified three days ago; the session list dialog is open in English
- **Steps:**
  1. Open the session list.
  2. Read the relative-time label on the three-day-old session.
- **Assertions:**
  - A1: The label reads exactly "3 days ago" (the `{days}` token is replaced by 3).
  - A2: No literal "{days}" token remains visible.
  - A3: A session modified today reads "Today" and one modified yesterday reads "Yesterday".
- **Traces:** rows 171, 172; M11

### WUI-UIX-06: {param} substitution survives translation — German "vor {days} Tagen"
- **Persona:** German-speaking user viewing relative session timestamps
- **Preconditions:** The active language is German; a saved session was last modified three days ago; the session list is open
- **Steps:**
  1. Open the session list.
  2. Read the relative-time label on the three-day-old session.
- **Assertions:**
  - A1: The label reads exactly "vor 3 Tagen" (the German template "vor {days} Tagen" with `{days}`=3).
  - A2: No literal "{days}" token remains.
  - A3: A multi-token message (e.g. a file-size error) substitutes every distinct named token in the German string, not just the first.
- **Traces:** rows 171, 172; M11

### WUI-UIX-07: Switching back to English restores identity strings
- **Persona:** User who tried German and switched back to English
- **Preconditions:** The app is currently in German with translated strings visible
- **Steps:**
  1. Switch the active language back to English (`setLanguage("en")`).
  2. Observe the editor placeholder, Send action, and Settings title.
- **Assertions:**
  - A1: The placeholder returns to "Type a message...".
  - A2: The Send action returns to "Send"; Settings returns to "Settings".
  - A3: A "3 days ago" relative label is shown in English again (identity lookup; key === displayed text).
- **Traces:** rows 171, 172; M11

### WUI-UIX-08: formatUsage produces the ↑ ↓ R W $ summary in order
- **Persona:** Power user reading the per-turn stats bar after an assistant reply
- **Preconditions:** A turn completed with input=1500, output=2300, cacheRead=12000, cacheWrite=800 tokens, and total cost 0.0123
- **Steps:**
  1. Read the per-turn usage summary in the stats bar.
- **Assertions:**
  - A1: The summary reads "↑1.5k ↓2.3k R12k W800 $0.0123".
  - A2: The parts appear in exactly that order, space-separated.
  - A3: The cost segment is the trailing component and uses four decimal places.
- **Traces:** row 173; M11

### WUI-UIX-09: formatUsage omits absent/zero components
- **Persona:** User on a turn that used only input and output tokens with no cache and no reported cost
- **Preconditions:** A completed turn with input=500, output=900, no cacheRead, no cacheWrite, no cost total
- **Steps:**
  1. Read the per-turn usage summary.
- **Assertions:**
  - A1: The summary shows only "↑500 ↓900" (counts under 1000 render as the raw integer, no "k").
  - A2: No "R", "W", or "$" segments are present.
  - A3: When usage data is entirely absent for a turn, the summary area shows no usage string at all (empty).
- **Traces:** row 173; M11

### WUI-UIX-10: formatTokenCount thresholds — raw, one-decimal k, rounded k
- **Persona:** User comparing small, medium, and large token figures
- **Preconditions:** Three reference values are observable through usage rendering: 999, 1500, and 23456 tokens
- **Steps:**
  1. Trigger usage displays that include each of the three token values.
- **Assertions:**
  - A1: 999 renders as "999" (below 1000 → raw integer, no suffix).
  - A2: 1500 renders as "1.5k" (1000–9999 → one decimal place with "k").
  - A3: 23456 renders as "23k" (≥10000 → rounded to nearest thousand with "k").
- **Traces:** row 173; M11

### WUI-UIX-11: formatCost always shows four decimal places, including zero
- **Persona:** User clicking into the cost detail for a turn
- **Preconditions:** Two turns are observable — one with total cost 0 and one with total cost 1.2 (dollars)
- **Steps:**
  1. Read the formatted cost for each turn.
- **Assertions:**
  - A1: A zero cost renders as "$0.0000".
  - A2: A cost of 1.2 renders as "$1.2000".
  - A3: The dollar sign always prefixes the figure and exactly four decimals are always shown.
- **Traces:** row 173; M11

### WUI-UIX-12: formatModelCost shows "$in/$out" and the localized Free label
- **Persona:** User scanning per-model pricing in the model selector
- **Preconditions:** The model selector is open; it lists a paid model (input 3.0, output 15.0 per million) and a zero-rate model
- **Steps:**
  1. Read the price label on the paid model.
  2. Read the price label on the zero-rate model in English, then switch to German and re-read it.
- **Assertions:**
  - A1: The paid model's price reads "$3/$15" (compact, trailing zeros trimmed).
  - A2: The zero-rate model (both rates 0, or no cost data) reads "Free" in English.
  - A3: After switching to German, the zero-rate model's price reads "Kostenlos".
- **Traces:** rows 143, 173; M8/M11

### WUI-UIX-13: formatModelCost compaction across magnitude bands
- **Persona:** User comparing models with widely varying prices
- **Preconditions:** The model selector is open with models priced at input 0.25 / output 0.5, input 1.5, input 12.0, and input 120.0 per million
- **Steps:**
  1. Read each model's compact price label.
- **Assertions:**
  - A1: A rate of 0.25 renders as "$0.25" (sub-1 → up to three decimals, trailing zeros trimmed).
  - A2: A rate of 1.5 renders as "$1.5"; a rate of 12.0 renders as "$12"; a rate of 120.0 renders as "$120".
  - A3: No price label shows trailing ".0" or superfluous trailing zeros.
- **Traces:** rows 143, 173; M8/M11

### WUI-UIX-14: formatTokens M/K context sizing in the model selector
- **Persona:** User comparing context-window sizes in the model list
- **Preconditions:** The model selector is open with models whose context windows are 8000, 128000, and 1000000 tokens
- **Steps:**
  1. Read the context-size figure for each listed model.
- **Assertions:**
  - A1: 8000 renders as "8" (the picker appends its own "K" separator) — i.e. a bare thousands figure with no decimal.
  - A2: 128000 renders as "128".
  - A3: 1000000 renders as "1M".
- **Traces:** rows 143, 173; M8/M11

### WUI-UIX-15: Theme toggle flips light↔dark and updates its own icon and tooltip
- **Persona:** User who prefers dark mode at night
- **Preconditions:** The app is loaded in light theme; the header theme toggle is visible showing a moon icon
- **Steps:**
  1. Click the theme toggle.
  2. Observe the toggle icon and tooltip, then click it again.
- **Assertions:**
  - A1: After the first click the document switches to dark theme (the `data-theme` attribute on the root element becomes "dark").
  - A2: In dark theme the toggle shows a sun icon and its tooltip reads "Switch to light theme".
  - A3: After the second click the theme returns to light, `data-theme` becomes "light", the icon returns to the moon, and the tooltip reads "Switch to dark theme".
- **Traces:** rows 175, 176; M11

### WUI-UIX-16: Theme choice persists across reloads
- **Persona:** Returning user who set dark mode previously
- **Preconditions:** The app is loaded; the user toggles to dark theme
- **Steps:**
  1. Toggle the theme to dark.
  2. Fully reload the application.
- **Assertions:**
  - A1: The toggled theme is saved to local settings under the "theme" key.
  - A2: After reload, the app comes up in dark theme (root `data-theme="dark"`) without any further interaction.
  - A3: The header toggle reflects the restored state (sun icon, "Switch to light theme" tooltip).
- **Traces:** rows 175, 176; M11

### WUI-UIX-17: OS prefers-color-scheme is honored until the toggle overrides it
- **Persona:** User whose operating system is set to dark mode and who has never used the toggle
- **Preconditions:** The OS reports `prefers-color-scheme: dark`; no explicit theme has been chosen by this user (no forced `data-theme` from a prior choice)
- **Steps:**
  1. Open the app fresh with no stored theme preference and the root not forced to light.
  2. Then click the theme toggle to switch to light.
- **Assertions:**
  - A1: With no explicit override, the app renders in dark colors because the OS prefers dark (the dark token set applies via the prefers-color-scheme path).
  - A2: After the user forces light (root `data-theme="light"`), the light token set applies even though the OS still prefers dark — the explicit choice wins.
  - A3: Forcing `data-theme="dark"` applies the dark token set regardless of OS preference.
- **Traces:** rows 175, 176; M11

### WUI-UIX-18: Background/foreground tokens resolve correctly in both themes
- **Persona:** User comparing the app chrome between light and dark
- **Preconditions:** The app is loaded; the user can switch between light and dark themes
- **Steps:**
  1. In light theme, observe the page background and primary text color.
  2. Switch to dark theme and observe the same surfaces.
- **Assertions:**
  - A1: In light theme the page background is white and the foreground text is near-black.
  - A2: In dark theme the page background is near-black and the foreground text is near-white.
  - A3: The transition is driven by the design tokens (background/foreground) and applies app-wide (body and `#app`), not just to a single component.
- **Traces:** rows 176, 178; M11

### WUI-UIX-19: Card, muted, and border tokens resolve in both themes
- **Persona:** User reading dialog cards, secondary text, and dividers in each theme
- **Preconditions:** A dialog or settings panel is open showing card surfaces, muted helper text, and border separators
- **Steps:**
  1. In light theme, observe a card surface, a piece of muted/secondary text, and a divider border.
  2. Switch to dark theme and observe the same elements.
- **Assertions:**
  - A1: Card surfaces use the card token (white in light, near-black in dark) and remain distinct from the surrounding background.
  - A2: Muted helper text is a mid-gray that is legible in both themes (lighter gray in dark, darker gray in light).
  - A3: Borders/dividers use the border token (light gray in light, dark gray in dark) and stay visible against their background in both themes.
- **Traces:** rows 176, 179; M11

### WUI-UIX-20: Primary and destructive tokens resolve in both themes
- **Persona:** User interacting with a primary action and a destructive action
- **Preconditions:** A surface is visible with a primary (default) button and a destructive button (e.g. a delete confirmation)
- **Steps:**
  1. Observe the primary button and the destructive button in light theme.
  2. Switch to dark theme and observe the same buttons.
- **Assertions:**
  - A1: The primary button uses the primary token (blue) with readable primary-foreground text (white) in both themes.
  - A2: The destructive button uses the destructive token (red) with white text in both themes.
  - A3: Both buttons keep sufficient contrast against the page background after the theme switch.
- **Traces:** rows 176, 174; M11

### WUI-UIX-21: Thinking-block shimmer animates while streaming and stops when done
- **Persona:** User watching a reasoning model think before answering
- **Preconditions:** A reasoning-capable model is selected; a prompt that triggers visible thinking is sent
- **Steps:**
  1. Send the prompt and watch the thinking block header during the reasoning phase.
  2. Wait until the turn completes.
- **Assertions:**
  - A1: While the model is streaming its thinking, the thinking-block header shows a moving shimmer effect (a gradient sweeping across the header text).
  - A2: The shimmer loops continuously for the duration of the streaming phase.
  - A3: Once streaming finishes, the shimmer stops and the header becomes static.
- **Traces:** rows 177, 40; M11

### WUI-UIX-22: Scrollbars are thin and track the theme
- **Persona:** User scrolling a long conversation or a tall dialog
- **Preconditions:** A scrollable region (e.g. the message history) has enough content to overflow
- **Steps:**
  1. Scroll the overflowing region and observe the scrollbar.
  2. Switch themes and observe the scrollbar color.
- **Assertions:**
  - A1: The scrollbar is thin (a slim track/thumb, not the OS-default wide bar).
  - A2: The scrollbar track is transparent and the thumb uses the border token color.
  - A3: After a theme switch the thumb color changes to match the new theme's border token; hovering the thumb darkens/brightens it toward the muted-foreground color.
- **Traces:** rows 178, 179; M11

### WUI-UIX-23: User-message bubble shows the brand-neutral gradient derived from the primary token
- **Persona:** User reviewing their own message bubbles in the transcript
- **Preconditions:** A conversation has at least one user message rendered
- **Steps:**
  1. Observe a user message bubble in light theme.
  2. Switch to dark theme and observe the same bubble.
- **Assertions:**
  - A1: The user message bubble has a subtle diagonal gradient background tinted from the primary (blue) token, with a faint primary-tinted border.
  - A2: The gradient is translucent (mixed with transparency) so it reads as a tint over the page, not a solid block.
  - A3: Because the gradient is derived from the primary token, it shifts appropriately when the theme changes (it is not a hardcoded fixed-color palette).
- **Traces:** row 178; M11

### WUI-UIX-24: Button variants and sizes render and respond to clicks
- **Persona:** User interacting with default, ghost, outline, and destructive buttons
- **Preconditions:** A surface exposes buttons in several variants and sizes; one button has a click handler bound
- **Steps:**
  1. Observe a default, a ghost, an outline, and a destructive button.
  2. Click an enabled button, then observe a disabled button.
- **Assertions:**
  - A1: The default button has a filled primary background; the destructive button has a filled red background; the outline button has a visible border with a transparent fill; the ghost button has no border/fill until hovered.
  - A2: Clicking an enabled button invokes its action exactly once.
  - A3: A disabled button is dimmed (reduced opacity) and does not respond to clicks (pointer events are suppressed).
- **Traces:** row 174; M11

### WUI-UIX-25: CopyButton copies its payload and confirms with a transient check
- **Persona:** User copying a code block or tool output to the clipboard
- **Preconditions:** A copy button is shown next to a copyable payload; the active language is English
- **Steps:**
  1. Click the copy button.
  2. Observe the icon/label immediately after, then again after ~1.5 seconds.
- **Assertions:**
  - A1: The button's text payload is written to the system clipboard.
  - A2: Immediately after the click the icon swaps to a check mark and (when its text label is shown) the label reads "Copied!".
  - A3: After about 1.5 seconds the icon reverts to the copy icon and the label returns to "Copy"; with the language set to German the labels read "Kopiert!" then "Kopieren".
- **Traces:** row 174; M11

### WUI-UIX-26: DownloadButton triggers a file download of its content
- **Persona:** User saving generated content (e.g. an artifact) to disk
- **Preconditions:** A download button is shown with a defined filename, mime type, and content payload
- **Steps:**
  1. Click the download button.
- **Assertions:**
  - A1: A browser file download is initiated for the configured filename.
  - A2: The downloaded file's content matches the button's payload and uses the configured mime type (text or binary).
  - A3: The button's tooltip resolves through translation (e.g. "Download" / "Herunterladen"), not a hardcoded brand string.
- **Traces:** row 174; M11

### WUI-UIX-27: Input primitive reflects typed value and fires input/change
- **Persona:** User typing into a settings field (e.g. a proxy URL)
- **Preconditions:** An Input primitive is rendered bound to a value with input and change handlers
- **Steps:**
  1. Type characters into the field.
  2. Commit the field (blur or press Enter where applicable).
  3. Observe a disabled instance of the same field.
- **Assertions:**
  - A1: The field shows the typed characters and reports each keystroke via its input handler.
  - A2: On commit, the change handler fires with the final value.
  - A3: The placeholder text uses the muted-foreground token and resolves through translation; a disabled field is dimmed and does not accept input.
- **Traces:** rows 43, 174; M2/M11

### WUI-UIX-28: Select primitive lists options, marks the active one, and reports changes
- **Persona:** User choosing a thinking level from a dropdown
- **Preconditions:** A Select primitive is rendered with options (e.g. Off, Minimal, Low, Medium, High) and a current value
- **Steps:**
  1. Open the select and review the options.
  2. Choose a different option.
- **Assertions:**
  - A1: All provided options are listed, and the currently selected value is marked as selected.
  - A2: Choosing a new option fires the change handler with that option's value.
  - A3: With the language set to German the option labels render translated (e.g. "Off"→"Aus", "Medium"→"Mittel").
- **Traces:** row 174; M11

### WUI-UIX-29: Switch primitive toggles and reports its boolean state
- **Persona:** User enabling the document-fetch proxy via a switch
- **Preconditions:** A Switch primitive is rendered reflecting an off state with a change handler bound
- **Steps:**
  1. Click the switch to turn it on.
  2. Click it again to turn it off.
  3. Observe a disabled switch instance.
- **Assertions:**
  - A1: The switch presents as a checkable control with the switch role and the primary accent color when on.
  - A2: Each click fires the change handler with the new boolean state (true then false).
  - A3: A disabled switch is dimmed and does not toggle on click.
- **Traces:** row 174; M11

### WUI-UIX-30: Badge renders a primary-colored pill with its content
- **Persona:** User seeing a count or status pill (e.g. the floating artifacts badge)
- **Preconditions:** A Badge is rendered with text content (e.g. a number)
- **Steps:**
  1. Observe the badge.
  2. Switch themes and observe it again.
- **Assertions:**
  - A1: The badge is a rounded pill with a primary background and primary-foreground text.
  - A2: The badge content (e.g. a count) is centered and legible.
  - A3: Because it uses the primary/primary-foreground tokens, its colors remain consistent and legible after a theme switch.
- **Traces:** rows 174, 176; M11

### WUI-UIX-31: Label renders and associates with its control
- **Persona:** User reading a form field label and clicking it to focus the field
- **Preconditions:** A Label is rendered with a `for` reference to an input's id
- **Steps:**
  1. Observe the label text and styling.
  2. Click the label.
- **Assertions:**
  - A1: The label uses the foreground token with medium weight at the small text size.
  - A2: The label text resolves through translation (German when the language is German).
  - A3: Clicking the label focuses the associated input (its `for` matches the input id).
- **Traces:** row 174; M11

### WUI-UIX-32: Every visible string resolves through i18n — no hardcoded brand text
- **Persona:** Privacy- and brand-conscious user inspecting the UI in both languages
- **Preconditions:** The app is loaded; the user can switch languages and navigate the major surfaces (header, editor, settings tabs, dialogs, model selector)
- **Steps:**
  1. Walk the header, message editor, settings dialog (Providers & Models, Proxy, API Keys tabs), session list, and model selector in English.
  2. Switch to German and walk the same surfaces.
- **Assertions:**
  - A1: In German, the previously English labels across all walked surfaces switch to their German equivalents (no English remnants where a German override exists).
  - A2: No visible label exposes a raw translation key as a placeholder artifact (e.g. no stray "{n}" / "{provider}" tokens after substitution).
  - A3: No surface shows a third-party brand or vendor name; all product-facing copy is the vendored, brand-neutral wording.
- **Traces:** rows 171, 172, 199; M11
