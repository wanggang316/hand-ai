// <custom-provider-card> — one configured custom provider in the settings tab.
//
// For auto-discovery providers it shows a connection status indicator
// (connected / checking / disconnected) plus the live model count; for manual
// providers it shows the count of persisted models. Refresh (auto-discovery
// only) / Edit / Delete actions are delegated to the parent tab via callbacks.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property } from "lit/decorators.js";
import type { CustomProvider } from "../storage/backend";
import { Button } from "../ui/button";
import { i18n } from "../utils/i18n";

export type ProviderStatusKind = "connected" | "checking" | "disconnected";

export interface ProviderStatus {
  modelCount: number;
  status: ProviderStatusKind;
}

@customElement("custom-provider-card")
export class CustomProviderCard extends LitElement {
  @property({ type: Object }) provider!: CustomProvider;
  @property({ type: Boolean }) isAutoDiscovery = false;
  @property({ type: Object }) status?: ProviderStatus;
  @property({ attribute: false }) onRefresh?: (provider: CustomProvider) => void;
  @property({ attribute: false }) onEdit?: (provider: CustomProvider) => void;
  @property({ attribute: false }) onDelete?: (provider: CustomProvider) => void;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  private renderStatus(): TemplateResult {
    if (!this.isAutoDiscovery) {
      return html`<div class="text-xs text-muted-foreground mt-1">
        ${i18n("Models")}: ${this.provider.models?.length ?? 0}
      </div>`;
    }
    if (!this.status) return html``;

    const dot =
      this.status.status === "connected"
        ? html`<span class="text-green-500">●</span>`
        : this.status.status === "checking"
          ? html`<span class="text-yellow-500">●</span>`
          : html`<span class="text-red-500">●</span>`;

    const text =
      this.status.status === "connected"
        ? `${this.status.modelCount} ${i18n("models")}`
        : this.status.status === "checking"
          ? i18n("Checking...")
          : i18n("Disconnected");

    return html`<div class="text-xs text-muted-foreground mt-1 flex items-center gap-1">${dot} ${text}</div>`;
  }

  override render(): TemplateResult {
    return html`
      <div class="border border-border rounded-lg p-4 space-y-2">
        <div class="flex items-center justify-between gap-2">
          <div class="flex-1 min-w-0">
            <div class="font-medium text-sm text-foreground truncate">${this.provider.name}</div>
            <div class="text-xs text-muted-foreground mt-1">
              <span class="capitalize">${this.provider.type}</span>
              ${this.provider.baseUrl ? html` • ${this.provider.baseUrl}` : ""}
            </div>
            ${this.renderStatus()}
          </div>
          <div class="flex gap-1 flex-shrink-0">
            ${this.isAutoDiscovery && this.onRefresh
              ? Button({
                  variant: "ghost",
                  size: "sm",
                  onClick: () => this.onRefresh?.(this.provider),
                  children: i18n("Refresh"),
                })
              : ""}
            ${this.onEdit
              ? Button({
                  variant: "ghost",
                  size: "sm",
                  onClick: () => this.onEdit?.(this.provider),
                  children: i18n("Edit"),
                })
              : ""}
            ${this.onDelete
              ? Button({
                  variant: "ghost",
                  size: "sm",
                  onClick: () => this.onDelete?.(this.provider),
                  children: i18n("Delete"),
                })
              : ""}
          </div>
        </div>
      </div>
    `;
  }
}
