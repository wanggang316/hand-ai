// Markdown renderer element. Parses its `content` to safe HTML via the local,
// dependency-free `renderMarkdown` (escape-first, so source HTML cannot survive)
// and renders it into the light DOM so Tailwind utility classes apply. Used by
// assistant/user/thinking message bodies and the Markdown artifact preview.

import { html, LitElement } from "lit";
import { customElement, property } from "lit/decorators.js";
import { unsafeHTML } from "lit/directives/unsafe-html.js";
import { renderMarkdown } from "./markdown";

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
    return html`<div class="markdown-body break-words">
      ${unsafeHTML(renderMarkdown(this.content))}
    </div>`;
  }
}
