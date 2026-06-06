// <custom-provider-dialog> — add / edit a custom LLM provider.
//
// Auto-discovery types (Ollama / llama.cpp / vLLM / LM Studio) expose a Test
// Connection button that runs `discoverModels` and previews the first 5
// results; manual API-shape types persist their (initially empty) model list
// for later editing. Saving writes through `CustomProvidersStore`. Base URLs
// are prefilled with the per-type default when empty.

import { html, type TemplateResult } from "lit";
import { customElement, state } from "lit/decorators.js";
import type { Model } from "../core/model";
import { getAppStorage } from "../storage/app-storage";
import type { AutoDiscoveryProviderType, CustomProvider, CustomProviderType } from "../storage/backend";
import { Button } from "../ui/button";
import { DialogBase } from "../ui/dialog-base";
import { i18n } from "../utils/i18n";
import { DEFAULT_BASE_URLS, discoverModels } from "./discovery";

const AUTO_DISCOVERY_TYPES: ReadonlySet<CustomProviderType> = new Set<CustomProviderType>([
  "ollama",
  "llama.cpp",
  "vllm",
  "lmstudio",
]);

const PROVIDER_TYPE_OPTIONS: { value: CustomProviderType; label: string }[] = [
  { value: "ollama", label: "Ollama (auto-discovery)" },
  { value: "llama.cpp", label: "llama.cpp (auto-discovery)" },
  { value: "vllm", label: "vLLM (auto-discovery)" },
  { value: "lmstudio", label: "LM Studio (auto-discovery)" },
  { value: "openai-completions", label: "OpenAI Completions Compatible" },
  { value: "openai-responses", label: "OpenAI Responses Compatible" },
  { value: "anthropic-messages", label: "Anthropic Messages Compatible" },
];

@customElement("custom-provider-dialog")
export class CustomProviderDialog extends DialogBase {
  private editing?: CustomProvider;
  private initialType?: CustomProviderType;
  private onSaveCallback?: () => void;

  @state() private name = "";
  @state() private type: CustomProviderType = "openai-completions";
  @state() private baseUrl = "";
  @state() private apiKey = "";
  @state() private testing = false;
  @state() private testError = "";
  @state() private discoveredModels: Model[] = [];

  protected override modalWidth = "min(800px, 90vw)";
  protected override modalHeight = "min(700px, 90vh)";

  static open(
    provider: CustomProvider | undefined,
    initialType: CustomProviderType | undefined,
    onSave?: () => void,
  ): CustomProviderDialog {
    const dialog = new CustomProviderDialog();
    dialog.editing = provider;
    dialog.initialType = initialType;
    dialog.onSaveCallback = onSave;
    dialog.initializeFromProvider();
    dialog.open();
    return dialog;
  }

  private initializeFromProvider(): void {
    if (this.editing) {
      this.name = this.editing.name;
      this.type = this.editing.type;
      this.baseUrl = this.editing.baseUrl;
      this.apiKey = this.editing.apiKey ?? "";
      this.discoveredModels = this.editing.models ?? [];
    } else {
      this.name = "";
      this.type = this.initialType ?? "openai-completions";
      this.baseUrl = "";
      this.prefillBaseUrl();
      this.apiKey = "";
      this.discoveredModels = [];
    }
    this.testError = "";
    this.testing = false;
  }

  private prefillBaseUrl(): void {
    if (this.baseUrl) return;
    if (this.isAutoDiscoveryType()) {
      this.baseUrl = DEFAULT_BASE_URLS[this.type as AutoDiscoveryProviderType] ?? "";
    }
  }

  private isAutoDiscoveryType(): boolean {
    return AUTO_DISCOVERY_TYPES.has(this.type);
  }

  private async testConnection(): Promise<void> {
    if (!this.isAutoDiscoveryType()) return;
    this.testing = true;
    this.testError = "";
    this.discoveredModels = [];
    try {
      const models = await discoverModels(
        this.type as AutoDiscoveryProviderType,
        this.baseUrl,
        this.apiKey || undefined,
      );
      this.discoveredModels = models.map((m) => ({ ...m, provider: this.name || this.type }));
    } catch (err) {
      this.testError = err instanceof Error ? err.message : String(err);
      this.discoveredModels = [];
    } finally {
      this.testing = false;
      this.requestUpdate();
    }
  }

  private async save(): Promise<void> {
    if (!this.name || !this.baseUrl) {
      this.testError = i18n("Please fill in all required fields");
      this.requestUpdate();
      return;
    }
    try {
      const provider: CustomProvider = {
        id: this.editing?.id ?? crypto.randomUUID(),
        name: this.name,
        type: this.type,
        baseUrl: this.baseUrl,
        apiKey: this.apiKey || undefined,
        // Auto-discovery types fetch models on demand; manual types persist.
        models: this.isAutoDiscoveryType() ? undefined : (this.editing?.models ?? []),
      };
      await getAppStorage().customProviders.set(provider);
      this.onSaveCallback?.();
      this.close();
    } catch (err) {
      console.error("Failed to save provider:", err);
      this.testError = i18n("Failed to save provider");
      this.requestUpdate();
    }
  }

  private field(label: string, control: TemplateResult): TemplateResult {
    return html`<div class="flex flex-col gap-2">
      <label class="text-sm font-medium text-foreground">${label}</label>
      ${control}
    </div>`;
  }

  private textInput(
    value: string,
    placeholder: string,
    onInput: (v: string) => void,
    type: "text" | "password" = "text",
  ): TemplateResult {
    return html`<input
      type=${type}
      class="rounded-md border border-border bg-background px-2 h-9 text-sm text-foreground outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring"
      .value=${value}
      placeholder=${placeholder}
      @input=${(e: Event) => onInput((e.target as HTMLInputElement).value)}
    />`;
  }

  protected override renderContent(): TemplateResult {
    return html`
      <div class="flex flex-col h-full overflow-hidden">
        <div class="p-6 flex-shrink-0 border-b border-border">
          <h2 class="text-lg font-semibold text-foreground">
            ${this.editing ? i18n("Edit Provider") : i18n("Add Provider")}
          </h2>
        </div>

        <div class="flex-1 overflow-y-auto p-6">
          <div class="flex flex-col gap-4">
            ${this.field(
              i18n("Provider Name"),
              this.textInput(this.name, i18n("e.g., My Ollama Server"), (v) => {
                this.name = v;
              }),
            )}
            ${this.field(
              i18n("Provider Type"),
              html`<select
                class="rounded-md border border-border bg-background px-2 h-9 text-sm text-foreground outline-none cursor-pointer"
                .value=${this.type}
                @change=${(e: Event) => {
                  this.type = (e.target as HTMLSelectElement).value as CustomProviderType;
                  this.baseUrl = "";
                  this.prefillBaseUrl();
                  this.discoveredModels = [];
                  this.testError = "";
                  this.requestUpdate();
                }}
              >
                ${PROVIDER_TYPE_OPTIONS.map(
                  (opt) =>
                    html`<option value=${opt.value} ?selected=${opt.value === this.type}>${opt.label}</option>`,
                )}
              </select>`,
            )}
            ${this.field(
              i18n("Base URL"),
              this.textInput(this.baseUrl, i18n("e.g., http://localhost:11434"), (v) => {
                this.baseUrl = v;
                this.requestUpdate();
              }),
            )}
            ${this.field(
              i18n("API Key (Optional)"),
              this.textInput(
                this.apiKey,
                i18n("Leave empty if not required"),
                (v) => {
                  this.apiKey = v;
                },
                "password",
              ),
            )}
            ${this.isAutoDiscoveryType()
              ? html`<div class="flex flex-col gap-2">
                  ${Button({
                    variant: "outline",
                    disabled: this.testing || !this.baseUrl,
                    onClick: () => void this.testConnection(),
                    children: this.testing ? i18n("Testing...") : i18n("Test Connection"),
                  })}
                  ${this.testError ? html`<div class="text-sm text-destructive">${this.testError}</div>` : ""}
                  ${this.discoveredModels.length > 0
                    ? html`<div class="text-sm text-muted-foreground">
                        ${i18n("Discovered")} ${this.discoveredModels.length} ${i18n("models")}:
                        <ul class="list-disc list-inside mt-2">
                          ${this.discoveredModels.slice(0, 5).map((m) => html`<li>${m.name}</li>`)}
                          ${this.discoveredModels.length > 5
                            ? html`<li>...${i18n("and")} ${this.discoveredModels.length - 5} ${i18n("more")}</li>`
                            : ""}
                        </ul>
                      </div>`
                    : ""}
                </div>`
              : html`<div class="text-sm text-muted-foreground">
                  ${i18n("For manual provider types, add models after saving the provider.")}
                </div>`}
          </div>
        </div>

        <div class="p-6 flex-shrink-0 border-t border-border flex justify-end gap-2">
          ${Button({ variant: "ghost", onClick: () => this.close(), children: i18n("Cancel") })}
          ${Button({
            variant: "default",
            disabled: !this.name || !this.baseUrl,
            onClick: () => void this.save(),
            children: i18n("Save"),
          })}
        </div>
      </div>
    `;
  }
}
