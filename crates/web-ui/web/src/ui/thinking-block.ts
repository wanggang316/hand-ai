// Collapsible reasoning block. Minimal M1 version: a toggle header (with a
// shimmer class while streaming) over a markdown body. The rich version (full
// shimmer keyframes, markdown thinking styles) is finalized in the theming
// milestone; the shape and props are stable so it can be enhanced in place.

import { html, LitElement } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { ChevronRight } from "lucide";
import { icon } from "./icons";
import "./markdown-block";

@customElement("thinking-block")
export class ThinkingBlock extends LitElement {
  @property() content = "";
  @property({ type: Boolean }) isStreaming = false;
  @state() private isExpanded = false;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  private toggleExpanded() {
    this.isExpanded = !this.isExpanded;
  }

  override render() {
    const shimmer = this.isStreaming
      ? "animate-shimmer bg-gradient-to-r from-muted-foreground via-foreground to-muted-foreground bg-[length:200%_100%] bg-clip-text text-transparent"
      : "";

    return html`
      <div class="thinking-block">
        <div
          class="thinking-header cursor-pointer select-none flex items-center gap-2 py-1 text-sm text-muted-foreground hover:text-foreground transition-colors"
          @click=${this.toggleExpanded}
        >
          <span class="transition-transform inline-block ${this.isExpanded ? "rotate-90" : ""}">
            ${icon(ChevronRight, "sm")}
          </span>
          <span class=${shimmer}>Thinking...</span>
        </div>
        ${this.isExpanded
          ? html`<markdown-block .content=${this.content} .isThinking=${true}></markdown-block>`
          : ""}
      </div>
    `;
  }
}
