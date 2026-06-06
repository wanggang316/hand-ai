// <proxy-tab> — a SettingsTab for the document-fetch proxy.
//
// This configures the proxy used by the browser-side `extract_document` tool to
// fetch remote documents that would otherwise be blocked by CORS. It is NOT an
// LLM proxy: LLM calls are made server-side and have no browser CORS constraint.
//
// Settings persist via SettingsStore under `proxy.enabled` (boolean) and
// `proxy.url` (string). The proxy must accept requests as `<proxy-url>/?url=<target-url>`.

import { html, type TemplateResult } from "lit";
import { customElement, state } from "lit/decorators.js";
import { getAppStorage } from "../storage/app-storage";
import { i18n } from "../utils/i18n";
import { SettingsTab } from "./settings-tab";

export const PROXY_ENABLED_KEY = "proxy.enabled";
export const PROXY_URL_KEY = "proxy.url";
const DEFAULT_PROXY_URL = "http://localhost:3001";

@customElement("proxy-tab")
export class ProxyTab extends SettingsTab {
  readonly id = "proxy";
  readonly label = i18n("Proxy");

  @state() private proxyEnabled = false;
  @state() private proxyUrl = DEFAULT_PROXY_URL;

  override async connectedCallback(): Promise<void> {
    super.connectedCallback();
    try {
      const storage = getAppStorage();
      const enabled = await storage.settings.get<boolean>(PROXY_ENABLED_KEY);
      const url = await storage.settings.get<string>(PROXY_URL_KEY);
      if (enabled !== null) this.proxyEnabled = enabled;
      if (url !== null && url !== "") this.proxyUrl = url;
    } catch (err) {
      console.error("Failed to load proxy settings:", err);
    }
  }

  private async save(): Promise<void> {
    try {
      const storage = getAppStorage();
      await storage.settings.set(PROXY_ENABLED_KEY, this.proxyEnabled);
      await storage.settings.set(PROXY_URL_KEY, this.proxyUrl);
    } catch (err) {
      console.error("Failed to save proxy settings:", err);
    }
  }

  protected override renderContent(): TemplateResult {
    return html`
      <div class="flex flex-col gap-4">
        <p class="text-sm text-muted-foreground">
          ${i18n("Lets the in-browser document fetcher bypass CORS restrictions when extracting remote documents. This does not affect LLM calls, which are made server-side.")}
        </p>

        <label class="flex items-center justify-between cursor-pointer">
          <span class="text-sm font-medium text-foreground">${i18n("Use document-fetch proxy")}</span>
          <input
            type="checkbox"
            class="h-4 w-4 cursor-pointer accent-primary"
            .checked=${this.proxyEnabled}
            @change=${(e: Event) => {
              this.proxyEnabled = (e.target as HTMLInputElement).checked;
              void this.save();
            }}
          />
        </label>

        <div class="flex flex-col gap-2">
          <label class="text-sm font-medium text-foreground">${i18n("Proxy URL")}</label>
          <input
            type="text"
            class="rounded-md border border-border bg-background px-2 h-9 text-sm text-foreground outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring disabled:opacity-50"
            .value=${this.proxyUrl}
            ?disabled=${!this.proxyEnabled}
            placeholder=${DEFAULT_PROXY_URL}
            @input=${(e: Event) => {
              this.proxyUrl = (e.target as HTMLInputElement).value;
            }}
            @change=${() => void this.save()}
          />
          <p class="text-xs text-muted-foreground">
            ${i18n("Format: the proxy must accept requests as <proxy-url>/?url=<target-url>")}
          </p>
        </div>
      </div>
    `;
  }
}
