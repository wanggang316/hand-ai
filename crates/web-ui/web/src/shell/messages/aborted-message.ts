// Aborted-message stub. Renders the italic "Request aborted" notice shown when a
// turn was cancelled mid-stream.

import { html, LitElement } from "lit";
import { customElement } from "lit/decorators.js";
import { i18n } from "../../utils/i18n";

@customElement("aborted-message")
export class AbortedMessage extends LitElement {
  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  protected override render() {
    return html`<span class="text-sm text-destructive italic">${i18n("Request aborted")}</span>`;
  }
}
