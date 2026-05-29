// <providers-models-tab> — provider management surface for the settings dialog.
//
// Two sections: cloud providers (per-provider API-key inputs) and custom
// providers (add / edit / refresh / delete, UUID-keyed in IndexedDB). The cloud
// provider list is derived from the distinct provider names in the server's
// model catalog (`get_available_models`) so it always matches what the running
// server actually supports; a small static fallback is used when no agent is
// wired in. Auto-discovery providers get a live status probe on load and on
// refresh.

import { html, type TemplateResult } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import type { RemoteAgent } from "../client/remote-agent";
import { SettingsTab } from "../dialogs/settings-tab";
import { getAppStorage } from "../storage/app-storage";
import type { AutoDiscoveryProviderType, CustomProvider, CustomProviderType } from "../storage/backend";
import { i18n } from "../utils/i18n";
import "./custom-provider-card";
import type { ProviderStatus } from "./custom-provider-card";
import { CustomProviderDialog } from "./custom-provider-dialog";
import { discoverModels } from "./discovery";
import "./provider-key-input";

const ADD_PROVIDER_OPTIONS: { value: CustomProviderType; label: string }[] = [
  { value: "ollama", label: "Ollama" },
  { value: "llama.cpp", label: "llama.cpp" },
  { value: "vllm", label: "vLLM" },
  { value: "lmstudio", label: "LM Studio" },
  { value: "openai-completions", label: "OpenAI Completions Compatible" },
  { value: "openai-responses", label: "OpenAI Responses Compatible" },
  { value: "anthropic-messages", label: "Anthropic Messages Compatible" },
];

// Fallback cloud-provider list used only when no agent catalog is available.
const FALLBACK_CLOUD_PROVIDERS = ["anthropic", "openai", "google", "groq", "openrouter", "xai"];

function isAutoDiscovery(type: CustomProviderType): boolean {
  return type === "ollama" || type === "llama.cpp" || type === "vllm" || type === "lmstudio";
}

@customElement("providers-models-tab")
export class ProvidersModelsTab extends SettingsTab {
  readonly id = "providers-models";
  readonly label = i18n("Providers & Models");

  /** Source of the cloud-provider list and the key-save catalog refresh. */
  @property({ attribute: false }) agent?: RemoteAgent;

  @state() private cloudProviders: string[] = FALLBACK_CLOUD_PROVIDERS;
  @state() private customProviders: CustomProvider[] = [];
  @state() private providerStatus = new Map<string, ProviderStatus>();

  override async connectedCallback(): Promise<void> {
    super.connectedCallback();
    await Promise.all([this.loadCloudProviders(), this.loadCustomProviders()]);
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

  private async loadCustomProviders(): Promise<void> {
    try {
      this.customProviders = await getAppStorage().customProviders.getAll();
      this.requestUpdate();
      for (const provider of this.customProviders) {
        if (isAutoDiscovery(provider.type)) void this.probeProvider(provider);
      }
    } catch (err) {
      console.error("Failed to load custom providers:", err);
    }
  }

  private async probeProvider(provider: CustomProvider): Promise<void> {
    this.providerStatus.set(provider.id, { modelCount: 0, status: "checking" });
    this.requestUpdate();
    try {
      const models = await discoverModels(
        provider.type as AutoDiscoveryProviderType,
        provider.baseUrl,
        provider.apiKey,
      );
      this.providerStatus.set(provider.id, { modelCount: models.length, status: "connected" });
    } catch {
      this.providerStatus.set(provider.id, { modelCount: 0, status: "disconnected" });
    }
    this.requestUpdate();
  }

  private async addCustomProvider(type: CustomProviderType): Promise<void> {
    CustomProviderDialog.open(undefined, type, () => void this.loadCustomProviders());
  }

  private async editProvider(provider: CustomProvider): Promise<void> {
    CustomProviderDialog.open(provider, undefined, () => void this.loadCustomProviders());
  }

  private async deleteProvider(provider: CustomProvider): Promise<void> {
    try {
      await getAppStorage().customProviders.delete(provider.id);
      this.providerStatus.delete(provider.id);
      await this.loadCustomProviders();
    } catch (err) {
      console.error("Failed to delete provider:", err);
    }
  }

  private renderCloud(): TemplateResult {
    return html`
      <div class="flex flex-col gap-6">
        <div>
          <h3 class="text-sm font-semibold text-foreground mb-2">${i18n("Cloud Providers")}</h3>
          <p class="text-sm text-muted-foreground mb-4">
            ${i18n("Cloud LLM providers with predefined models. API keys are stored locally in your browser.")}
          </p>
        </div>
        <div class="flex flex-col gap-6">
          ${this.cloudProviders.map(
            (provider) =>
              html`<provider-key-input .provider=${provider} .agent=${this.agent}></provider-key-input>`,
          )}
        </div>
      </div>
    `;
  }

  private renderCustom(): TemplateResult {
    return html`
      <div class="flex flex-col gap-6">
        <div class="flex items-center justify-between gap-2">
          <div>
            <h3 class="text-sm font-semibold text-foreground mb-2">${i18n("Custom Providers")}</h3>
            <p class="text-sm text-muted-foreground">
              ${i18n("User-configured servers with auto-discovered or manually defined models.")}
            </p>
          </div>
          <select
            class="rounded-md border border-border bg-transparent px-2 h-8 text-xs text-foreground outline-none cursor-pointer hover:bg-muted flex-shrink-0"
            @change=${(e: Event) => {
              const sel = e.target as HTMLSelectElement;
              const value = sel.value as CustomProviderType;
              sel.value = ""; // reset to placeholder after picking
              if (value) void this.addCustomProvider(value);
            }}
          >
            <option value="" selected>${i18n("Add Provider")}</option>
            ${ADD_PROVIDER_OPTIONS.map((opt) => html`<option value=${opt.value}>${opt.label}</option>`)}
          </select>
        </div>

        ${this.customProviders.length === 0
          ? html`<div class="text-sm text-muted-foreground text-center py-8">
              ${i18n("No custom providers configured. Click 'Add Provider' to get started.")}
            </div>`
          : html`<div class="flex flex-col gap-4">
              ${this.customProviders.map(
                (provider) => html`<custom-provider-card
                  .provider=${provider}
                  .isAutoDiscovery=${isAutoDiscovery(provider.type)}
                  .status=${this.providerStatus.get(provider.id)}
                  .onRefresh=${(p: CustomProvider) => void this.probeProvider(p)}
                  .onEdit=${(p: CustomProvider) => void this.editProvider(p)}
                  .onDelete=${(p: CustomProvider) => void this.deleteProvider(p)}
                ></custom-provider-card>`,
              )}
            </div>`}
      </div>
    `;
  }

  protected override renderContent(): TemplateResult {
    return html`
      <div class="flex flex-col gap-8">
        ${this.renderCloud()}
        <div class="border-t border-border"></div>
        ${this.renderCustom()}
      </div>
    `;
  }
}
