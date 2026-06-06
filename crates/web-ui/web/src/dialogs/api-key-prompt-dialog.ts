// <api-key-prompt-dialog> — prompts for a single provider's API key.
//
// `prompt(provider)` resolves `true` once a key for that provider is stored
// (detected by polling ProviderKeysStore), and `false` if the dialog is closed
// without one. The body reuses `<provider-key-input>` (M8), which performs the
// actual save; this dialog just waits for the key to appear and then closes.

import { html, type TemplateResult } from "lit";
import { customElement, property } from "lit/decorators.js";
import "../providers/provider-key-input";
import { getAppStorage } from "../storage/app-storage";
import { i18n } from "../utils/i18n";
import { DialogBase } from "../ui/dialog-base";

const POLL_INTERVAL_MS = 500;

@customElement("api-key-prompt-dialog")
export class ApiKeyPromptDialog extends DialogBase {
  @property() provider = "";

  private resolvePromise?: (success: boolean) => void;
  private pollTimer?: ReturnType<typeof setInterval>;

  protected override modalWidth = "min(500px, 90vw)";

  /** Open the dialog and resolve when a key is stored (true) or cancelled (false). */
  static prompt(provider: string): Promise<boolean> {
    const dialog = new ApiKeyPromptDialog();
    dialog.provider = provider;
    dialog.open();
    return new Promise<boolean>((resolve) => {
      dialog.resolvePromise = resolve;
    });
  }

  override connectedCallback(): void {
    super.connectedCallback();
    // Poll for key presence; resolve + close once the user has saved one.
    this.pollTimer = setInterval(() => {
      void this.checkForKey();
    }, POLL_INTERVAL_MS);
  }

  private async checkForKey(): Promise<void> {
    let hasKey = false;
    try {
      hasKey = await getAppStorage().providerKeys.has(this.provider);
    } catch (err) {
      console.error("Failed to poll provider key:", err);
      return;
    }
    if (hasKey) {
      this.clearPoll();
      if (this.resolvePromise) {
        this.resolvePromise(true);
        this.resolvePromise = undefined;
      }
      this.close();
    }
  }

  private clearPoll(): void {
    if (this.pollTimer !== undefined) {
      clearInterval(this.pollTimer);
      this.pollTimer = undefined;
    }
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.clearPoll();
  }

  override close(): void {
    this.clearPoll();
    super.close();
    // If we closed without a stored key, the prompt was cancelled.
    if (this.resolvePromise) {
      this.resolvePromise(false);
      this.resolvePromise = undefined;
    }
  }

  protected override renderContent(): TemplateResult {
    return html`
      <div class="flex flex-col gap-4 p-6">
        <div>
          <h2 class="text-lg font-semibold text-foreground">${i18n("API Key Required")}</h2>
          <p class="text-sm text-muted-foreground mt-1">
            ${i18n("Enter an API key for {provider} to continue.").replace("{provider}", this.provider)}
          </p>
        </div>
        <provider-key-input .provider=${this.provider}></provider-key-input>
        <div class="flex justify-end">
          <button
            class="h-8 px-3 text-xs inline-flex items-center justify-center rounded-md font-medium hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
            @click=${() => this.close()}
          >
            ${i18n("Cancel")}
          </button>
        </div>
      </div>
    `;
  }
}
