// Raw tool-call debug view. Shows the call arguments and the result (text +
// details) as JSON/text code-blocks. Used as a low-level inspector for tool
// activity when a renderer's rich view is not enough.

import { html, LitElement } from "lit";
import { customElement, property } from "lit/decorators.js";
import type { ToolResultMessage } from "../../core/messages";
import { i18n } from "../../utils/i18n";
import "../../ui/code-block";

@customElement("tool-message-debug")
export class ToolMessageDebugView extends LitElement {
  @property({ type: Object }) callArgs: unknown;
  @property({ type: Object }) result?: ToolResultMessage;
  @property({ type: Boolean }) hasResult = false;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  private pretty(value: unknown): { content: string; isJson: boolean } {
    try {
      if (typeof value === "string") {
        const maybeJson = JSON.parse(value);
        return { content: JSON.stringify(maybeJson, null, 2), isJson: true };
      }
      return { content: JSON.stringify(value, null, 2), isJson: true };
    } catch {
      return { content: typeof value === "string" ? value : String(value), isJson: false };
    }
  }

  override render() {
    const textOutput =
      this.result?.content
        ?.filter((c) => c.type === "text")
        .map((c) => c.text ?? "")
        .join("\n") || "";
    const output = this.pretty(textOutput);
    const details = this.pretty(this.result?.details);

    return html`
      <div class="mt-3 flex flex-col gap-2">
        <div>
          <div class="text-xs font-medium mb-1 text-muted-foreground">${i18n("Call")}</div>
          <code-block .code=${this.pretty(this.callArgs).content} language="json"></code-block>
        </div>
        <div>
          <div class="text-xs font-medium mb-1 text-muted-foreground">${i18n("Result")}</div>
          ${this.hasResult
            ? html`<code-block .code=${output.content} language=${output.isJson ? "json" : "text"}></code-block>
                <code-block .code=${details.content} language=${details.isJson ? "json" : "text"}></code-block>`
            : html`<div class="text-xs text-muted-foreground">${i18n("(no result)")}</div>`}
        </div>
      </div>
    `;
  }
}
