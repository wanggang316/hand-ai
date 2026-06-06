// Single tool-call card. Resolves the tool renderer for the call's name and
// renders the params + result. When the turn was aborted with no result, it
// synthesizes an error stub so the renderer can show the aborted state. The
// renderer decides whether it owns its chrome (`isCustom`, no card wrapper) or
// should be wrapped in the default card.

import { html, LitElement } from "lit";
import { customElement, property } from "lit/decorators.js";
import type { ToolCall, ToolResultMessage } from "../../core/messages";
import type { AgentTool } from "../../core/tool";
import { renderTool } from "../../tools/renderer-registry";

@customElement("tool-message")
export class ToolMessage extends LitElement {
  @property({ type: Object }) toolCall!: ToolCall;
  @property({ type: Object }) tool?: AgentTool;
  @property({ type: Object }) result?: ToolResultMessage;
  @property({ type: Boolean }) pending = false;
  @property({ type: Boolean }) aborted = false;
  @property({ type: Boolean }) isStreaming = false;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  override render() {
    const toolName = this.tool?.name || this.toolCall.name;

    // Synthesize an error result for an aborted, result-less tool call so the
    // renderer can show the error state.
    const result: ToolResultMessage | undefined = this.aborted
      ? {
          role: "toolResult",
          isError: true,
          content: [],
          toolCallId: this.toolCall.id,
        }
      : this.result;

    const renderResult = renderTool(
      toolName,
      this.toolCall.arguments,
      result,
      !this.aborted && (this.isStreaming || this.pending),
    );

    // Custom renderers own their chrome; render bare.
    if (renderResult.isCustom) {
      return renderResult.content;
    }

    // Default: wrap in a card.
    return html`
      <div class="p-2.5 border border-border rounded-md bg-card text-card-foreground shadow-xs">
        ${renderResult.content}
      </div>
    `;
  }
}
