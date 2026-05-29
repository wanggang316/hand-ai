// Stable conversation history renderer. Renders the committed message list with
// a keyed repeat() so already-rendered messages are not re-created during
// streaming. Artifact-role messages are skipped (UI persistence only); tool
// results are paired to their tool call by toolCallId; in-flight tool calls are
// hidden while streaming so the streaming container and this list never render
// the same card twice.

import { html, LitElement, type TemplateResult } from "lit";
import { property } from "lit/decorators.js";
import { repeat } from "lit/directives/repeat.js";
import type { AgentMessage, ToolResultMessage } from "../core/messages";
import type { AgentTool } from "../core/tool";
import { renderHistoryMessage } from "./render-message";

export class MessageList extends LitElement {
  @property({ type: Array }) messages: AgentMessage[] = [];
  @property({ type: Array }) tools: AgentTool[] = [];
  @property({ type: Object }) pendingToolCalls?: ReadonlySet<string>;
  @property({ type: Boolean }) isStreaming = false;
  @property({ attribute: false }) onCostClick?: () => void;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this; // light DOM so Tailwind classes apply
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  private buildRenderItems(): Array<{ key: string; template: TemplateResult }> {
    // Pair tool results to their call id for inline rendering in assistant msgs.
    const resultByCallId = new Map<string, ToolResultMessage>();
    for (const message of this.messages) {
      if (message.role === "toolResult") {
        resultByCallId.set(message.toolCallId, message);
      }
    }

    const items: Array<{ key: string; template: TemplateResult }> = [];
    let index = 0;
    for (const msg of this.messages) {
      const template = renderHistoryMessage(msg, {
        isStreaming: false,
        tools: this.tools,
        pendingToolCalls: this.pendingToolCalls,
        toolResultsById: resultByCallId,
        onCostClick: this.onCostClick,
        // While streaming, hide pending tool calls here; the streaming
        // container owns the in-flight rendering.
        hidePendingToolCalls: this.isStreaming,
      });
      if (template) {
        items.push({ key: `msg:${index}`, template });
        index++;
      }
    }
    return items;
  }

  override render() {
    const items = this.buildRenderItems();
    return html`<div class="flex flex-col gap-3">
      ${repeat(
        items,
        (it) => it.key,
        (it) => it.template,
      )}
    </div>`;
  }
}

if (!customElements.get("message-list")) {
  customElements.define("message-list", MessageList);
}
