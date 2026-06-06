// Assistant-message renderer. Renders the assistant's content blocks in the
// order they appear — markdown text, thinking blocks, and tool-call cards — then
// the usage summary (once streaming has finished), an error box, or the aborted
// stub. Tool-call cards delegate to <tool-message>, which resolves the renderer.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property } from "lit/decorators.js";
import type { AssistantMessage as AssistantMessageType, ToolResultMessage } from "../../core/messages";
import type { AgentTool } from "../../core/tool";
import { formatUsage } from "../../utils/format";
import { i18n } from "../../utils/i18n";
import "../../ui/markdown-block";
import "./thinking-block";
import "./tool-message";

@customElement("assistant-message")
export class AssistantMessage extends LitElement {
  @property({ type: Object }) message!: AssistantMessageType;
  @property({ type: Array }) tools?: AgentTool[];
  @property({ type: Object }) pendingToolCalls?: ReadonlySet<string>;
  @property({ type: Boolean }) hideToolCalls = false;
  @property({ type: Object }) toolResultsById?: Map<string, ToolResultMessage>;
  @property({ type: Boolean }) isStreaming = false;
  @property({ type: Boolean }) hidePendingToolCalls = false;
  @property({ attribute: false }) onCostClick?: () => void;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  override render() {
    const orderedParts: TemplateResult[] = [];

    for (const chunk of this.message.content) {
      if (chunk.type === "text" && chunk.text.trim() !== "") {
        orderedParts.push(html`<markdown-block .content=${chunk.text}></markdown-block>`);
      } else if (chunk.type === "thinking" && chunk.thinking.trim() !== "") {
        orderedParts.push(
          html`<thinking-block .content=${chunk.thinking} .isStreaming=${this.isStreaming}></thinking-block>`,
        );
      } else if (chunk.type === "toolCall") {
        if (this.hideToolCalls) continue;
        const tool = this.tools?.find((t) => t.name === chunk.name);
        const pending = this.pendingToolCalls?.has(chunk.id) ?? false;
        const result = this.toolResultsById?.get(chunk.id);
        // Skip in-flight tool calls when hidePendingToolCalls is set so the
        // streaming container and the stable list never render the same card.
        if (this.hidePendingToolCalls && pending && !result) {
          continue;
        }
        // Aborted: the message stopped and there is no result for this call.
        const aborted = this.message.stopReason === "aborted" && !result;
        orderedParts.push(
          html`<tool-message
            .tool=${tool}
            .toolCall=${chunk}
            .result=${result}
            .pending=${pending}
            .aborted=${aborted}
            .isStreaming=${this.isStreaming}
          ></tool-message>`,
        );
      }
    }

    return html`
      <div>
        ${orderedParts.length ? html`<div class="px-4 flex flex-col gap-3">${orderedParts}</div>` : ""}
        ${this.message.usage && !this.isStreaming
          ? this.onCostClick
            ? html`<div
                class="px-4 mt-2 text-xs text-muted-foreground cursor-pointer hover:text-foreground transition-colors"
                @click=${this.onCostClick}
              >
                ${formatUsage(this.message.usage)}
              </div>`
            : html`<div class="px-4 mt-2 text-xs text-muted-foreground">${formatUsage(this.message.usage)}</div>`
          : ""}
        ${this.message.stopReason === "error" && this.message.errorMessage
          ? html`<div class="mx-4 mt-3 p-3 bg-destructive/10 text-destructive rounded-lg text-sm overflow-hidden">
              <strong>${i18n("Error:")}</strong> ${this.message.errorMessage}
            </div>`
          : ""}
        ${this.message.stopReason === "aborted"
          ? html`<span class="mx-4 text-sm text-destructive italic">${i18n("Request aborted")}</span>`
          : ""}
      </div>
    `;
  }
}
