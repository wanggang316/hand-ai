// Monospace code block with a copy button. Syntax highlighting is plain for now
// (full highlighting lands in the theming milestone); the element renders the
// code verbatim in a <pre> with horizontal scrolling and a header copy action.
// Contract matches its call sites: `<code-block .code=${string} language="json">`.

import { html, LitElement } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { Check, Copy } from "lucide";
import { icon } from "./icons";
import { i18n } from "../utils/i18n";

@customElement("code-block")
export class CodeBlock extends LitElement {
  @property() code = "";
  @property() language = "text";
  @state() private copied = false;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this; // light DOM so Tailwind classes apply
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  private async copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(this.code || "");
      this.copied = true;
      setTimeout(() => {
        this.copied = false;
      }, 1500);
    } catch (e) {
      console.error("Copy failed", e);
    }
  }

  override render() {
    return html`
      <div class="border border-border rounded-lg overflow-hidden">
        <div class="flex items-center justify-between px-3 py-1.5 bg-muted border-b border-border">
          <span class="text-xs text-muted-foreground font-mono">${this.language}</span>
          <button
            @click=${() => this.copy()}
            class="flex items-center gap-1 px-2 py-0.5 text-xs rounded hover:bg-accent text-muted-foreground hover:text-accent-foreground transition-colors"
            title=${i18n("Copy")}
          >
            ${this.copied ? icon(Check, "sm") : icon(Copy, "sm")}
            ${this.copied ? html`<span>${i18n("Copied!")}</span>` : ""}
          </button>
        </div>
        <div class="overflow-auto">
          <pre class="m-0 p-3 text-xs font-mono whitespace-pre text-foreground">${this.code || ""}</pre>
        </div>
      </div>
    `;
  }
}
