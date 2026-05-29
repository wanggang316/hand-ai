// Reusable expandable accordion. Captures its light-DOM children on connect and
// re-inserts them inside the details area when expanded. Used by thinking blocks
// (and later by tool renderers). Light DOM so Tailwind utility classes apply.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { ChevronDown, ChevronRight } from "lucide";
import { icon } from "./icons";

@customElement("expandable-section")
export class ExpandableSection extends LitElement {
  @property() summary = "";
  @property({ type: Boolean }) defaultExpanded = false;
  @state() private expanded = false;
  private capturedChildren: Node[] = [];

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this; // light DOM
  }

  override connectedCallback(): void {
    super.connectedCallback();
    // Capture children before first render, then clear so render re-inserts them.
    this.capturedChildren = Array.from(this.childNodes);
    this.innerHTML = "";
    this.expanded = this.defaultExpanded;
  }

  override render(): TemplateResult {
    return html`
      <div>
        <button
          @click=${() => {
            this.expanded = !this.expanded;
          }}
          class="flex items-center gap-2 text-sm text-muted-foreground hover:text-foreground transition-colors w-full text-left"
        >
          ${icon(this.expanded ? ChevronDown : ChevronRight, "sm")}
          <span>${this.summary}</span>
        </button>
        ${this.expanded ? html`<div class="mt-2">${this.capturedChildren}</div>` : ""}
      </div>
    `;
  }
}
