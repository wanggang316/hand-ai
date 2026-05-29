// Live streaming-message renderer. Owns the in-flight assistant message and
// renders it with requestAnimationFrame batching. The message is deep-cloned on
// each assignment (structuredClone) so Lit's dirty check fires on mutated nested
// objects (e.g. a toolCall.arguments string growing during streaming). A pulsing
// cursor is shown before the first token and while streaming; it disappears once
// the message is cleared on message_end / agent_end.

import { html, LitElement } from "lit";
import { property, state } from "lit/decorators.js";
import type { AgentMessage, ToolResultMessage } from "../core/messages";
import type { AgentTool } from "../core/tool";
import { renderAssistantMessage } from "./render-message";

export class StreamingMessageContainer extends LitElement {
  @property({ type: Array }) tools: AgentTool[] = [];
  @property({ type: Boolean }) isStreaming = false;
  @property({ type: Object }) pendingToolCalls?: ReadonlySet<string>;
  @property({ type: Object }) toolResultsById?: Map<string, ToolResultMessage>;
  @property({ attribute: false }) onCostClick?: () => void;

  @state() private _message: AgentMessage | null = null;
  private _pendingMessage: AgentMessage | null = null;
  private _updateScheduled = false;
  private _immediateUpdate = false;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  /** Deep-clone so Lit detects nested mutations during streaming. */
  private clone(message: AgentMessage): AgentMessage {
    if (typeof structuredClone === "function") {
      return structuredClone(message);
    }
    return JSON.parse(JSON.stringify(message)) as AgentMessage;
  }

  /** Update the streamed message, batching non-immediate updates via rAF. */
  public setMessage(message: AgentMessage | null, immediate = false): void {
    this._pendingMessage = message;

    // Immediate path: clearing, or an explicit final update.
    if (immediate || message === null) {
      this._immediateUpdate = true;
      this._message = message ? this.clone(message) : null;
      this.requestUpdate();
      this._pendingMessage = null;
      this._updateScheduled = false;
      return;
    }

    // Batch streaming updates for performance.
    if (!this._updateScheduled) {
      this._updateScheduled = true;
      requestAnimationFrame(() => {
        if (!this._immediateUpdate && this._pendingMessage !== null) {
          this._message = this.clone(this._pendingMessage);
          this.requestUpdate();
        }
        this._pendingMessage = null;
        this._updateScheduled = false;
        this._immediateUpdate = false;
      });
    }
  }

  override render() {
    // No message yet: show a pulsing cursor while streaming, else nothing.
    if (!this._message) {
      if (this.isStreaming) {
        return html`<div class="flex flex-col gap-3 mb-3">
          <span class="mx-4 inline-block w-2 h-4 bg-muted-foreground animate-pulse"></span>
        </div>`;
      }
      return html``;
    }

    const msg = this._message;

    // User / toolResult messages are owned by the stable list, not here.
    if (msg.role !== "assistant") {
      return html``;
    }

    return html`
      <div class="flex flex-col gap-3 mb-3">
        ${renderAssistantMessage(msg, {
          isStreaming: this.isStreaming,
          pendingToolCalls: this.pendingToolCalls,
          toolResultsById: this.toolResultsById,
          hidePendingToolCalls: false,
        })}
        ${this.isStreaming
          ? html`<span class="mx-4 inline-block w-2 h-4 bg-muted-foreground animate-pulse"></span>`
          : ""}
      </div>
    `;
  }
}

if (!customElements.get("streaming-message-container")) {
  customElements.define("streaming-message-container", StreamingMessageContainer);
}
