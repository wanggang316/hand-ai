// Scrolling output pane with a copy button and an error variant. Used by tool
// renderers (bash output, REPL console, etc.) to show captured stdout/stderr.
// Auto-scrolls to the bottom on content changes so streaming output stays
// pinned. Contract: `<console-block .content=${string} .variant=${"error"}>`.

import { html, LitElement } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { Check, Copy } from "lucide";
import { icon } from "./icons";
import { i18n } from "../utils/i18n";

export type ConsoleBlockVariant = "default" | "error";

@customElement("console-block")
export class ConsoleBlock extends LitElement {
  @property() content = "";
  @property() variant: ConsoleBlockVariant = "default";
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
      await navigator.clipboard.writeText(this.content || "");
      this.copied = true;
      setTimeout(() => {
        this.copied = false;
      }, 1500);
    } catch (e) {
      console.error("Copy failed", e);
    }
  }

  override updated(): void {
    // Auto-scroll to bottom on content changes.
    const container = this.querySelector(".console-scroll") as HTMLElement | null;
    if (container) {
      container.scrollTop = container.scrollHeight;
    }
  }

  override render() {
    const isError = this.variant === "error";
    const textClass = isError ? "text-destructive" : "text-foreground";

    return html`
      <div class="border border-border rounded-lg overflow-hidden">
        <div class="flex items-center justify-between px-3 py-1.5 bg-muted border-b border-border">
          <span class="text-xs text-muted-foreground font-mono">${i18n("console")}</span>
          <button
            @click=${() => this.copy()}
            class="flex items-center gap-1 px-2 py-0.5 text-xs rounded hover:bg-accent text-muted-foreground hover:text-accent-foreground transition-colors"
            title=${i18n("Copy output")}
          >
            ${this.copied ? icon(Check, "sm") : icon(Copy, "sm")}
            ${this.copied ? html`<span>${i18n("Copied!")}</span>` : ""}
          </button>
        </div>
        <div class="console-scroll overflow-auto max-h-64">
          <pre
            class="m-0 p-3 text-xs ${textClass} font-mono whitespace-pre-wrap"
          >${this.content || ""}</pre>
        </div>
      </div>
    `;
  }
}
