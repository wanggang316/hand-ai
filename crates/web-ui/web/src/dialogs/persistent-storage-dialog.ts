// <persistent-storage-dialog> — asks the browser to persist IndexedDB storage.
//
// `request()` resolves `true` if persistent storage is (or becomes) granted,
// `false` otherwise. If storage is already persisted it resolves immediately
// without showing the dialog. When the StorageManager API is unsupported the
// dialog still shows but the action degrades gracefully with a clear message
// (no window.confirm/alert). The browser may grant `navigator.storage.persist()`
// silently; this dialog explains why persistence matters and lets the user
// trigger the request or continue without it.

import { html, type TemplateResult } from "lit";
import { customElement, state } from "lit/decorators.js";
import { Button } from "../ui/button";
import { i18n } from "../utils/i18n";
import { DialogBase } from "../ui/dialog-base";

@customElement("persistent-storage-dialog")
export class PersistentStorageDialog extends DialogBase {
  @state() private requesting = false;
  @state() private message = "";

  private resolvePromise?: (granted: boolean) => void;

  protected override modalWidth = "min(500px, 90vw)";

  /**
   * Request persistent storage. Resolves true if granted (or already granted),
   * false if denied, cancelled, or unsupported.
   */
  static async request(): Promise<boolean> {
    // Already persisted → resolve without a dialog.
    if (navigator.storage?.persisted) {
      try {
        if (await navigator.storage.persisted()) return true;
      } catch {
        // fall through to showing the dialog
      }
    }
    const dialog = new PersistentStorageDialog();
    dialog.open();
    return new Promise<boolean>((resolve) => {
      dialog.resolvePromise = resolve;
    });
  }

  private resolve(granted: boolean): void {
    if (this.resolvePromise) {
      this.resolvePromise(granted);
      this.resolvePromise = undefined;
    }
  }

  private async grant(): Promise<void> {
    if (!navigator.storage?.persist) {
      // Graceful fallback when the API is unavailable.
      this.message = i18n("Persistent storage is not supported in this browser. Your data is still saved locally but may be cleared under storage pressure.");
      this.requestUpdate();
      return;
    }
    this.requesting = true;
    this.message = "";
    try {
      const granted = await navigator.storage.persist();
      this.resolve(granted);
      super.close();
    } catch (err) {
      console.error("Failed to request persistent storage:", err);
      this.message = i18n("Could not request persistent storage. Your data is still saved locally.");
    } finally {
      this.requesting = false;
      this.requestUpdate();
    }
  }

  override close(): void {
    super.close();
    this.resolve(false);
  }

  protected override renderContent(): TemplateResult {
    return html`
      <div class="flex flex-col gap-4 p-6">
        <div>
          <h2 class="text-lg font-semibold text-foreground">${i18n("Storage Permission")}</h2>
          <p class="text-sm text-muted-foreground mt-1">
            ${i18n("Allow persistent storage so your conversations are not cleared when the browser needs disk space.")}
          </p>
        </div>

        <ul class="text-sm text-muted-foreground list-disc list-inside space-y-1">
          <li>${i18n("Your conversations are saved locally in your browser.")}</li>
          <li>${i18n("Data will not be deleted automatically to free up space.")}</li>
          <li>${i18n("No data is sent to external servers.")}</li>
        </ul>

        ${this.message
          ? html`<div class="text-sm text-foreground bg-muted rounded-md p-3">${this.message}</div>`
          : ""}

        <div class="flex gap-3 justify-end">
          ${Button({
            variant: "outline",
            disabled: this.requesting,
            onClick: () => this.close(),
            children: i18n("Continue Anyway"),
          })}
          ${Button({
            variant: "default",
            disabled: this.requesting,
            onClick: () => void this.grant(),
            children: this.requesting ? i18n("Requesting...") : i18n("Grant Permission"),
          })}
        </div>
      </div>
    `;
  }
}
