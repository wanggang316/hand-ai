// <hand-model-selector> — the keyboard-navigable model picker dialog.
//
// Opened via the static `open(currentModel, onSelect, opts?)`. The model list
// comes from the server's `get_available_models` (via RemoteAgent), merged with
// any models discovered from custom providers in IndexedDB. Search is a
// subsequence-scored fuzzy match; Thinking and Vision capability filters narrow
// the list; arrow/enter/escape navigation is IME-safe; the current model gets a
// checkmark; an optional `allowedProviders` set restricts the candidate set.

import { html, type PropertyValues, type TemplateResult } from "lit";
import { customElement, state } from "lit/decorators.js";
import { createRef, ref } from "lit/directives/ref.js";
import { Brain, Check, Image as ImageIcon, Search } from "lucide";
import type { RemoteAgent } from "../client/remote-agent";
import type { Model } from "../core/model";
import { getAppStorage } from "../storage/app-storage";
import type { AutoDiscoveryProviderType } from "../storage/backend";
import { DialogBase } from "../ui/dialog-base";
import { icon } from "../ui/icons";
import { formatModelCost, formatTokens } from "../utils/format";
import { i18n } from "../utils/i18n";
import { discoverModels } from "./discovery";

/**
 * Score `query` against `text` by subsequence matching: every query char must
 * appear in order. A tighter match (fewer gaps between matched chars) scores
 * higher; returns 0 when the query is not a subsequence of the text.
 */
export function subsequenceScore(query: string, text: string): number {
  let qi = 0;
  let ti = 0;
  let gaps = 0;
  let lastMatchIndex = -1;

  while (qi < query.length && ti < text.length) {
    if (query[qi] === text[ti]) {
      if (lastMatchIndex >= 0) gaps += ti - lastMatchIndex - 1;
      lastMatchIndex = ti;
      qi++;
    }
    ti++;
  }

  if (qi < query.length) return 0;
  return query.length / (query.length + gaps);
}

/** Two models are the same when provider + id match. */
function modelsAreEqual(a: Model | null | undefined, b: Model | null | undefined): boolean {
  if (!a || !b) return false;
  return a.provider === b.provider && a.id === b.id;
}

export interface ModelSelectorOptions {
  /** Source of the catalog; required for the list to populate. */
  agent?: RemoteAgent;
  /** Restrict candidates to these provider names. */
  allowedProviders?: string[];
}

@customElement("hand-model-selector")
export class ModelSelector extends DialogBase {
  @state() currentModel: Model | null = null;
  @state() private searchQuery = "";
  @state() private filterThinking = false;
  @state() private filterVision = false;
  @state() private selectedIndex = 0;
  @state() private navigationMode: "mouse" | "keyboard" = "mouse";
  @state() private catalogModels: Model[] = [];
  @state() private customProviderModels: Model[] = [];

  private onSelectCallback?: (model: Model) => void;
  private agent?: RemoteAgent;
  private allowedProviders?: Set<string>;
  private scrollContainerRef = createRef<HTMLDivElement>();
  private searchInputRef = createRef<HTMLInputElement>();
  private lastMousePosition = { x: 0, y: 0 };

  protected override modalWidth = "min(400px, 90vw)";
  protected override modalHeight = "min(600px, 85vh)";

  static open(
    currentModel: Model | null,
    onSelect: (model: Model) => void,
    opts?: ModelSelectorOptions,
  ): ModelSelector {
    const selector = new ModelSelector();
    selector.currentModel = currentModel;
    selector.onSelectCallback = onSelect;
    selector.agent = opts?.agent;
    if (opts?.allowedProviders) {
      selector.allowedProviders = new Set(opts.allowedProviders);
    }
    selector.open();
    void selector.loadModels();
    return selector;
  }

  override async firstUpdated(changed: PropertyValues): Promise<void> {
    super.firstUpdated(changed);
    await this.updateComplete;
    this.searchInputRef.value?.focus();

    // Distinguish real mouse movement from layout-driven mouseenter so keyboard
    // navigation is not stolen by the item under a stationary cursor.
    this.addEventListener("mousemove", (e: MouseEvent) => {
      if (e.clientX === this.lastMousePosition.x && e.clientY === this.lastMousePosition.y) return;
      this.lastMousePosition = { x: e.clientX, y: e.clientY };
      if (this.navigationMode !== "keyboard") return;
      this.navigationMode = "mouse";
      const item = (e.target as HTMLElement).closest("[data-model-item]");
      if (!item) return;
      const all = this.scrollContainerRef.value?.querySelectorAll("[data-model-item]");
      if (!all) return;
      const idx = Array.from(all).indexOf(item);
      if (idx !== -1) this.selectedIndex = idx;
    });

    this.addEventListener("keydown", (e: KeyboardEvent) => {
      // IME-safe: never act on composition keystrokes.
      if (e.isComposing || e.key === "Process") return;
      const models = this.getFilteredModels();
      if (e.key === "ArrowDown") {
        e.preventDefault();
        this.navigationMode = "keyboard";
        this.selectedIndex = Math.min(this.selectedIndex + 1, models.length - 1);
        this.scrollToSelected();
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        this.navigationMode = "keyboard";
        this.selectedIndex = Math.max(this.selectedIndex - 1, 0);
        this.scrollToSelected();
      } else if (e.key === "Enter") {
        e.preventDefault();
        const chosen = models[this.selectedIndex];
        if (chosen) this.handleSelect(chosen.model);
      }
    });
  }

  private async loadModels(): Promise<void> {
    if (this.agent) {
      try {
        this.catalogModels = await this.agent.getAvailableModels();
      } catch (err) {
        console.error("Failed to load model catalog:", err);
        this.catalogModels = [];
      }
    }
    await this.loadCustomProviders();
    this.requestUpdate();
  }

  private async loadCustomProviders(): Promise<void> {
    const all: Model[] = [];
    try {
      const providers = await getAppStorage().customProviders.getAll();
      for (const provider of providers) {
        const isAutoDiscovery =
          provider.type === "ollama" ||
          provider.type === "llama.cpp" ||
          provider.type === "vllm" ||
          provider.type === "lmstudio";
        if (isAutoDiscovery) {
          try {
            const models = await discoverModels(
              provider.type as AutoDiscoveryProviderType,
              provider.baseUrl,
              provider.apiKey,
            );
            all.push(...models.map((m) => ({ ...m, provider: provider.name })));
          } catch (err) {
            console.debug(`Failed to load models from ${provider.name}:`, err);
          }
        } else if (provider.models) {
          all.push(...provider.models);
        }
      }
    } catch (err) {
      console.error("Failed to load custom providers:", err);
    } finally {
      this.customProviderModels = all;
    }
  }

  private handleSelect(model: Model): void {
    this.onSelectCallback?.(model);
    this.close();
  }

  private getFilteredModels(): Array<{ provider: string; id: string; model: Model }> {
    let entries = [...this.catalogModels, ...this.customProviderModels].map((model) => ({
      provider: model.provider,
      id: model.id,
      model,
    }));

    if (this.allowedProviders) {
      const allowed = this.allowedProviders;
      entries = entries.filter(({ provider }) => allowed.has(provider));
    }

    // Subsequence fuzzy search over "provider id name".
    if (this.searchQuery) {
      const query = this.searchQuery.toLowerCase().replace(/\s+/g, "");
      if (query) {
        const scored: Array<{ item: (typeof entries)[number]; score: number }> = [];
        for (const entry of entries) {
          const text = `${entry.provider} ${entry.id} ${entry.model.name}`.toLowerCase();
          const score = subsequenceScore(query, text);
          if (score > 0) scored.push({ item: entry, score });
        }
        scored.sort((a, b) => b.score - a.score);
        entries = scored.map((s) => s.item);
      }
    }

    if (this.filterThinking) entries = entries.filter(({ model }) => model.reasoning);
    if (this.filterVision) entries = entries.filter(({ model }) => model.input.includes("image"));

    // When not searching, float the current model first, then sort by provider.
    if (!this.searchQuery) {
      entries.sort((a, b) => {
        const aCurrent = modelsAreEqual(this.currentModel, a.model);
        const bCurrent = modelsAreEqual(this.currentModel, b.model);
        if (aCurrent && !bCurrent) return -1;
        if (!aCurrent && bCurrent) return 1;
        return a.provider.localeCompare(b.provider);
      });
    }

    return entries;
  }

  private scrollToSelected(): void {
    requestAnimationFrame(() => {
      const container = this.scrollContainerRef.value;
      const el = container?.querySelectorAll("[data-model-item]")[this.selectedIndex] as
        | HTMLElement
        | undefined;
      el?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    });
  }

  private resetScrollAndSelection(): void {
    this.selectedIndex = 0;
    if (this.scrollContainerRef.value) this.scrollContainerRef.value.scrollTop = 0;
  }

  private filterButton(active: boolean, label: string, glyph: TemplateResult, onToggle: () => void): TemplateResult {
    const cls = active
      ? "bg-primary text-primary-foreground"
      : "bg-secondary text-secondary-foreground hover:bg-muted";
    return html`<button
      class="inline-flex items-center gap-1 rounded-full px-3 h-8 text-xs font-medium transition-colors ${cls}"
      @click=${onToggle}
    >
      ${glyph} ${label}
    </button>`;
  }

  protected override renderContent(): TemplateResult {
    const models = this.getFilteredModels();

    return html`
      <div class="p-4 pb-3 flex flex-col gap-3 border-b border-border flex-shrink-0">
        <h2 class="text-base font-semibold text-foreground">${i18n("Select Model")}</h2>
        <div
          class="flex items-center gap-2 rounded-md border border-border bg-background px-2 h-9 focus-within:ring-1 focus-within:ring-ring"
        >
          ${icon(Search, "sm", "text-muted-foreground")}
          <input
            ${ref(this.searchInputRef)}
            type="text"
            class="flex-1 bg-transparent outline-none text-sm text-foreground placeholder:text-muted-foreground"
            placeholder=${i18n("Search models...")}
            .value=${this.searchQuery}
            @input=${(e: Event) => {
              this.searchQuery = (e.target as HTMLInputElement).value;
              this.resetScrollAndSelection();
            }}
          />
        </div>
        <div class="flex gap-2">
          ${this.filterButton(this.filterThinking, i18n("Thinking"), icon(Brain, "sm"), () => {
            this.filterThinking = !this.filterThinking;
            this.resetScrollAndSelection();
          })}
          ${this.filterButton(this.filterVision, i18n("Vision"), icon(ImageIcon, "sm"), () => {
            this.filterVision = !this.filterVision;
            this.resetScrollAndSelection();
          })}
        </div>
      </div>

      <div class="flex-1 overflow-y-auto" ${ref(this.scrollContainerRef)}>
        ${models.length === 0
          ? html`<div class="px-4 py-8 text-center text-sm text-muted-foreground">
              ${i18n("No models found")}
            </div>`
          : models.map(({ provider, id, model }, index) => {
              const isCurrent = modelsAreEqual(this.currentModel, model);
              const isSelected = index === this.selectedIndex;
              return html`
                <div
                  data-model-item
                  class="px-4 py-3 cursor-pointer border-b border-border ${this.navigationMode === "mouse"
                    ? "hover:bg-muted"
                    : ""} ${isSelected ? "bg-accent" : ""}"
                  @click=${() => this.handleSelect(model)}
                  @mouseenter=${() => {
                    if (this.navigationMode === "mouse") this.selectedIndex = index;
                  }}
                >
                  <div class="flex items-center justify-between gap-2 mb-1">
                    <div class="flex items-center gap-2 flex-1 min-w-0">
                      <span class="text-sm font-medium text-foreground truncate">${id}</span>
                      ${isCurrent ? html`<span class="text-green-500">${icon(Check, "sm")}</span>` : ""}
                    </div>
                    <span
                      class="inline-flex items-center rounded-full border border-border px-2 py-0.5 text-xs text-muted-foreground"
                      >${provider}</span
                    >
                  </div>
                  <div class="flex items-center justify-between text-xs text-muted-foreground">
                    <div class="flex items-center gap-2">
                      <span class=${model.reasoning ? "" : "opacity-30"} title=${i18n("Thinking")}
                        >${icon(Brain, "sm")}</span
                      >
                      <span class=${model.input.includes("image") ? "" : "opacity-30"} title=${i18n("Vision")}
                        >${icon(ImageIcon, "sm")}</span
                      >
                      <span>${formatTokens(model.contextWindow)}K/${formatTokens(model.maxTokens)}K</span>
                    </div>
                    <span>${formatModelCost(model.cost)}</span>
                  </div>
                </div>
              `;
            })}
      </div>
    `;
  }
}
