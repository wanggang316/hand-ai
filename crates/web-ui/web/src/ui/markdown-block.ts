// Minimal text block element. M1 needs to render user/assistant message text
// correctly; the full markdown renderer (with syntax highlighting and rich
// formatting) lands in a later milestone. This intentionally renders plain text
// with preserved whitespace and word wrapping so the message shape is correct;
// swapping in a real markdown parser later only changes this file.

import { html, LitElement } from "lit";
import { customElement, property } from "lit/decorators.js";

@customElement("markdown-block")
export class MarkdownBlock extends LitElement {
  @property() content = "";
  /** Marks thinking content; reserved for later styling differences. */
  @property({ type: Boolean }) isThinking = false;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this; // light DOM so Tailwind classes apply
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  override render() {
    return html`<div class="whitespace-pre-wrap break-words leading-relaxed">${this.content}</div>`;
  }
}
