// Smoke harness for the M9 dialog system. Lets a controller browser-verify that
// the settings dialog mounts with its tabs and that the session-list dialog
// mounts — without a backend and without relying on requestAnimationFrame.
//
// `runDialogsSmoke()` opens <settings-dialog> with two stub tabs and opens
// <session-list-dialog>, then reports what mounted. It cleans up after itself.

import { html, type TemplateResult } from "lit";
import { SessionListDialog } from "./session-list-dialog";
import { SettingsDialog } from "./settings-dialog";
import { SettingsTab } from "./settings-tab";

class StubTab extends SettingsTab {
  readonly id: string;
  readonly label: string;
  constructor(id: string, label: string) {
    super();
    this.id = id;
    this.label = label;
  }
  protected override renderContent(): TemplateResult {
    return html`<div data-stub=${this.id}>${this.label}</div>`;
  }
}

// Define the stub tab as a one-off custom element (idempotent across calls).
if (!customElements.get("dialogs-smoke-stub-tab")) {
  customElements.define("dialogs-smoke-stub-tab", StubTab);
}

export interface DialogsSmokeResult {
  settingsOpened: boolean;
  settingsTabCount: number;
  sessionDialogOpened: boolean;
}

/**
 * Open both dialogs, settle their Lit render via `updateComplete` (rAF-free),
 * and report what mounted. Cleans up the dialogs before resolving.
 */
export async function runDialogsSmoke(): Promise<DialogsSmokeResult> {
  // --- settings dialog with two stub tabs ---
  const tabs = [new StubTab("alpha", "Alpha"), new StubTab("beta", "Beta")];
  const settings = SettingsDialog.open(tabs);
  await settings.updateComplete?.catch?.(() => {});
  // Each tab element renders once mounted; let their content settle too.
  await new Promise<void>((resolve) => setTimeout(resolve, 0));
  const settingsEl = document.querySelector("settings-dialog");
  const settingsOpened = !!settingsEl && settingsEl.isConnected;
  // The stub content carries data-stub; one per mounted tab.
  const settingsTabCount = settingsEl
    ? settingsEl.querySelectorAll("[data-stub]").length
    : 0;
  settings.close();

  // --- session list dialog ---
  const sessions = SessionListDialog.open(
    () => {},
    () => {},
  );
  await sessions.updateComplete?.catch?.(() => {});
  const sessionEl = document.querySelector("session-list-dialog");
  const sessionDialogOpened = !!sessionEl && sessionEl.isConnected;
  sessions.close();

  return { settingsOpened, settingsTabCount, sessionDialogOpened };
}
