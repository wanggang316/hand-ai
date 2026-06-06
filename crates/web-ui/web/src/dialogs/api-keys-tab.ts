// <api-keys-tab> — a SettingsTab listing cloud-provider API-key inputs.
//
// The provider list is derived from the distinct provider names in the server's
// model catalog (`get_available_models`) so it always matches what the running
// server supports; a small static fallback is used when no agent is wired in.
// Each row is a `<provider-key-input>` (M8), which stores the key locally and
// triggers a server-side catalog refresh on save. Keys are resolved server-side
// from its own environment for real LLM calls; the browser never transmits them.

import { html, type TemplateResult } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import type { RemoteAgent } from "../client/remote-agent";
import "../providers/provider-key-input";
import { i18n } from "../utils/i18n";
import { SettingsTab } from "./settings-tab";

// Fallback cloud-provider list used only when no agent catalog is available.
const FALLBACK_CLOUD_PROVIDERS = ["anthropic", "openai", "google", "groq", "openrouter", "xai"];

@customElement("api-keys-tab")
export class ApiKeysTab extends SettingsTab {
  readonly id = "api-keys";
  readonly label = i18n("API Keys");

  /** Source of the cloud-provider list and the key-save catalog refresh. */
  @property({ attribute: false }) agent?: RemoteAgent;

  @state() private cloudProviders: string[] = FALLBACK_CLOUD_PROVIDERS;

  override async connectedCallback(): Promise<void> {
    super.connectedCallback();
    await this.loadCloudProviders();
  }

  private async loadCloudProviders(): Promise<void> {
    if (!this.agent) return;
    try {
      const models = await this.agent.getAvailableModels();
      const names = Array.from(new Set(models.map((m) => m.provider))).filter(Boolean);
      if (names.length > 0) {
        names.sort((a, b) => a.localeCompare(b));
        this.cloudProviders = names;
        this.requestUpdate();
      }
    } catch (err) {
      console.error("Failed to load cloud providers:", err);
    }
  }

  protected override renderContent(): TemplateResult {
    return html`
      <div class="flex flex-col gap-6">
        <p class="text-sm text-muted-foreground">
          ${i18n("Configure API keys for LLM providers. Keys are stored locally in your browser.")}
        </p>
        <div class="flex flex-col gap-6">
          ${this.cloudProviders.map(
            (provider) =>
              html`<provider-key-input
                .provider=${provider}
                .agent=${this.agent}
              ></provider-key-input>`,
          )}
        </div>
      </div>
    `;
  }
}
