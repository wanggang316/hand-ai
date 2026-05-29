// `<preview-code-toggle>` — a two-segment toggle between a rendered "Preview"
// and a raw "Code" view. Used by the HTML / SVG / Markdown artifact viewers in
// their header button rows. The current mode is held on the `mode` property and
// a `mode-change` CustomEvent (detail = "preview" | "code") is fired on switch
// so the host element can swap its rendering.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property } from "lit/decorators.js";
import { Code, Eye } from "lucide";
import { icon } from "./icons";
import { i18n } from "../utils/i18n";

export type PreviewCodeMode = "preview" | "code";

@customElement("preview-code-toggle")
export class PreviewCodeToggle extends LitElement {
  @property() mode: PreviewCodeMode = "preview";

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "inline-flex";
  }

  private setMode(mode: PreviewCodeMode) {
    if (this.mode === mode) return;
    this.mode = mode;
    this.dispatchEvent(
      new CustomEvent("mode-change", { detail: mode, bubbles: true, composed: true }),
    );
  }

  override render(): TemplateResult {
    const active = "bg-background text-foreground shadow-sm";
    const inactive = "text-muted-foreground hover:text-foreground";
    return html`
      <div class="inline-flex items-center gap-0.5 rounded-md bg-muted p-0.5">
        <button
          @click=${() => this.setMode("preview")}
          class="inline-flex items-center gap-1 px-2 py-1 text-xs rounded transition-colors ${
            this.mode === "preview" ? active : inactive
          }"
          title=${i18n("Preview")}
        >
          ${icon(Eye, "sm")}<span>${i18n("Preview")}</span>
        </button>
        <button
          @click=${() => this.setMode("code")}
          class="inline-flex items-center gap-1 px-2 py-1 text-xs rounded transition-colors ${
            this.mode === "code" ? active : inactive
          }"
          title=${i18n("Code")}
        >
          ${icon(Code, "sm")}<span>${i18n("Code")}</span>
        </button>
      </div>
    `;
  }
}

declare global {
  interface HTMLElementTagNameMap {
    "preview-code-toggle": PreviewCodeToggle;
  }
}
