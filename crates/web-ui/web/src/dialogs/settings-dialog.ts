// <settings-dialog> — a tabbed settings modal extending DialogBase.
//
// Hosts a set of SettingsTab instances. On desktop a left sidebar lists the
// tabs; on mobile a horizontal strip of buttons sits above the content. All
// tabs are mounted once and kept in the DOM; switching tabs only toggles
// `display:none` so each tab's state (and its single `connectedCallback`) is
// preserved across switches.

import { html, type TemplateResult } from "lit";
import { customElement, state } from "lit/decorators.js";
import { DialogBase } from "../ui/dialog-base";
import { i18n } from "../utils/i18n";
import type { SettingsTab } from "./settings-tab";

@customElement("settings-dialog")
export class SettingsDialog extends DialogBase {
  private tabs: SettingsTab[] = [];
  @state() private activeIndex = 0;

  protected override modalWidth = "min(1000px, 90vw)";
  protected override modalHeight = "min(800px, 90vh)";

  /** Mount and open a settings dialog hosting the given tabs. */
  static open(tabs: SettingsTab[]): SettingsDialog {
    const dialog = new SettingsDialog();
    dialog.tabs = tabs;
    dialog.open();
    return dialog;
  }

  private setActive(index: number): void {
    this.activeIndex = index;
  }

  private renderSidebarItem(tab: SettingsTab, index: number): TemplateResult {
    const active = this.activeIndex === index;
    const cls = active
      ? "bg-secondary text-foreground font-medium"
      : "text-muted-foreground hover:bg-secondary/50 hover:text-foreground";
    return html`<button
      class="w-full text-left px-4 py-3 rounded-md transition-colors ${cls}"
      @click=${() => this.setActive(index)}
    >
      ${tab.label}
    </button>`;
  }

  private renderMobileTab(tab: SettingsTab, index: number): TemplateResult {
    const active = this.activeIndex === index;
    const cls = active
      ? "border-b-2 border-primary text-foreground"
      : "text-muted-foreground hover:text-foreground";
    return html`<button
      class="px-3 py-2 text-sm font-medium transition-colors ${cls}"
      @click=${() => this.setActive(index)}
    >
      ${tab.label}
    </button>`;
  }

  protected override renderContent(): TemplateResult {
    return html`
      <div class="flex flex-col h-full overflow-hidden p-6">
        <div class="pb-4 flex-shrink-0">
          <h2 class="text-lg font-semibold text-foreground">${i18n("Settings")}</h2>
        </div>

        <!-- Mobile tab strip -->
        <div class="md:hidden flex flex-shrink-0 pb-4 overflow-x-auto">
          ${this.tabs.map((tab, index) => this.renderMobileTab(tab, index))}
        </div>

        <div class="flex flex-1 overflow-hidden">
          <!-- Desktop sidebar -->
          <div class="hidden md:block w-64 flex-shrink-0 space-y-1">
            ${this.tabs.map((tab, index) => this.renderSidebarItem(tab, index))}
          </div>

          <!-- Content: all tabs mounted, only the active one visible -->
          <div class="flex-1 overflow-y-auto md:pl-6">
            ${this.tabs.map(
              (tab, index) =>
                html`<div style=${`display: ${this.activeIndex === index ? "block" : "none"}`}>
                  ${tab}
                </div>`,
            )}
          </div>
        </div>
      </div>
    `;
  }
}
