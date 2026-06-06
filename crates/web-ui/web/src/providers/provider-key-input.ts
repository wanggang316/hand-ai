// <provider-key-input> — a single cloud-provider API-key row.
//
// Shows whether a key is stored (a checkmark) WITHOUT revealing its value: the
// presence check goes through `ProviderKeysStore.has` and the saved value is
// never read back into the input. Saving persists via `ProviderKeysStore` and
// then asks the agent to refresh its model catalog (`getAvailableModels`) so
// the new key is exercised by the server.
//
// API-key VALIDATION is a server round-trip, not an in-browser completion: the
// browser never holds or calls with the secret (keys are resolved server-side
// from the environment / store for real LLM calls). For now, "save + catalog
// refresh" is the validation signal. A dedicated `validate_api_key` WS command
// can be added later for an explicit pass/fail without changing this UI.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import type { RemoteAgent } from "../client/remote-agent";
import { getAppStorage } from "../storage/app-storage";
import { Button } from "../ui/button";
import { i18n } from "../utils/i18n";

@customElement("provider-key-input")
export class ProviderKeyInput extends LitElement {
  @property() provider = "";
  /** Optional agent; when present, a save triggers a catalog refresh. */
  @property({ attribute: false }) agent?: RemoteAgent;

  @state() private keyInput = "";
  @state() private saving = false;
  @state() private hasKey = false;
  @state() private failed = false;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override async connectedCallback(): Promise<void> {
    super.connectedCallback();
    await this.refreshKeyStatus();
  }

  private async refreshKeyStatus(): Promise<void> {
    try {
      this.hasKey = await getAppStorage().providerKeys.has(this.provider);
    } catch (err) {
      console.error("Failed to check key status:", err);
    }
  }

  private async saveKey(): Promise<void> {
    if (!this.keyInput) return;
    this.saving = true;
    this.failed = false;
    try {
      await getAppStorage().providerKeys.set(this.provider, this.keyInput);
      this.hasKey = true;
      this.keyInput = "";
      // Server round-trip validation: refresh the catalog so the new key is
      // exercised server-side. Failure here does not revert the saved key.
      try {
        await this.agent?.getAvailableModels();
      } catch (err) {
        console.debug("Model catalog refresh after key save failed:", err);
      }
    } catch (err) {
      console.error("Failed to save API key:", err);
      this.failed = true;
      setTimeout(() => {
        this.failed = false;
        this.requestUpdate();
      }, 5000);
    } finally {
      this.saving = false;
      this.requestUpdate();
    }
  }

  private async clearKey(): Promise<void> {
    try {
      await getAppStorage().providerKeys.delete(this.provider);
      this.hasKey = false;
      this.requestUpdate();
    } catch (err) {
      console.error("Failed to delete API key:", err);
    }
  }

  override render(): TemplateResult {
    return html`
      <div class="space-y-2">
        <div class="flex items-center gap-2">
          <span class="text-sm font-medium capitalize text-foreground">${this.provider}</span>
          ${this.saving
            ? html`<span class="text-xs text-muted-foreground">${i18n("Saving...")}</span>`
            : this.hasKey
              ? html`<span class="text-green-600 dark:text-green-400" title=${i18n("Key stored")}>✓</span>`
              : ""}
          ${this.failed
            ? html`<span class="text-xs text-destructive">${i18n("Failed to save")}</span>`
            : ""}
        </div>
        <div class="flex items-center gap-2">
          <input
            type="password"
            class="flex-1 rounded-md border border-border bg-background px-2 h-9 text-sm text-foreground outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring"
            placeholder=${this.hasKey ? "••••••••••••" : i18n("Enter API key")}
            .value=${this.keyInput}
            @input=${(e: Event) => {
              this.keyInput = (e.target as HTMLInputElement).value;
            }}
          />
          ${Button({
            variant: "default",
            size: "sm",
            disabled: !this.keyInput || this.saving,
            onClick: () => void this.saveKey(),
            children: i18n("Save"),
          })}
          ${this.hasKey
            ? Button({
                variant: "ghost",
                size: "sm",
                onClick: () => void this.clearKey(),
                children: i18n("Remove"),
              })
            : ""}
        </div>
      </div>
    `;
  }
}
