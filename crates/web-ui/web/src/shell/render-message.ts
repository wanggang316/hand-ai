// Minimal M1 message rendering. Renders user text, assistant text/thinking
// blocks, and skips tool-call/tool-result rendering (rich tool cards land in
// the message+tool-rendering milestone). This is intentionally a small set of
// pure render functions, not a registry: the next milestone introduces the
// registry and swaps these call sites without reworking the shell.

import { html, type TemplateResult } from "lit";
import type {
  AgentMessage,
  AssistantMessage,
  ContentBlock,
  TextContent,
  ToolCall,
  ToolResultMessage,
  UserMessage,
  UserMessageWithAttachments,
} from "../core/messages";
import "../ui/markdown-block";
import "../ui/thinking-block";

export interface AssistantRenderOptions {
  isStreaming: boolean;
  /** Tool-call ids currently executing (no result yet). */
  pendingToolCalls?: ReadonlySet<string>;
  /** Tool results keyed by their tool-call id, for pairing. */
  toolResultsById?: Map<string, ToolResultMessage>;
  /**
   * When true, in-flight tool calls (pending, no result) are hidden so the
   * streaming container and the stable list never render the same card twice.
   */
  hidePendingToolCalls?: boolean;
}

function userText(msg: UserMessage | UserMessageWithAttachments): string {
  if (typeof msg.content === "string") return msg.content;
  const blocks: readonly ContentBlock[] = msg.content;
  return blocks
    .filter((c): c is TextContent => c.type === "text")
    .map((c) => c.text)
    .join("");
}

export function renderUserMessage(
  msg: UserMessage | UserMessageWithAttachments,
): TemplateResult {
  return html`
    <div class="flex justify-start mx-4">
      <div class="user-message-container py-2 px-4 rounded-xl">
        <markdown-block .content=${userText(msg)}></markdown-block>
      </div>
    </div>
  `;
}

/**
 * Render an assistant message in content order. M1 renders text and thinking
 * blocks; tool calls render a compact placeholder so ordering is preserved
 * (the rich tool-call card is added in the next milestone).
 */
export function renderAssistantMessage(
  msg: AssistantMessage,
  opts: AssistantRenderOptions,
): TemplateResult {
  const parts: TemplateResult[] = [];

  for (const chunk of msg.content) {
    if (chunk.type === "text" && chunk.text.trim() !== "") {
      parts.push(html`<markdown-block .content=${chunk.text}></markdown-block>`);
    } else if (chunk.type === "thinking" && chunk.thinking.trim() !== "") {
      parts.push(
        html`<thinking-block .content=${chunk.thinking} .isStreaming=${opts.isStreaming}></thinking-block>`,
      );
    } else if (chunk.type === "toolCall") {
      const call = chunk as ToolCall;
      const pending = opts.pendingToolCalls?.has(call.id) ?? false;
      const result = opts.toolResultsById?.get(call.id);
      // Hide pending (in-flight) tool calls when requested so the streaming
      // container and the stable list never double-render the same card.
      if (opts.hidePendingToolCalls && pending && !result) {
        continue;
      }
      // M1 renders a compact tool-call placeholder in content order; the rich
      // tool-call card is added in the next milestone.
      parts.push(
        html`<div class="text-xs text-muted-foreground font-mono">${call.name}(…)</div>`,
      );
    }
  }

  return html`
    <div>
      ${parts.length ? html`<div class="px-4 flex flex-col gap-3">${parts}</div>` : ""}
      ${msg.stopReason === "error" && msg.errorMessage
        ? html`<div class="mx-4 mt-3 p-3 bg-destructive/10 text-destructive rounded-lg text-sm overflow-hidden">
            <strong>Error:</strong> ${msg.errorMessage}
          </div>`
        : ""}
      ${msg.stopReason === "aborted"
        ? html`<span class="mx-4 text-sm text-destructive italic">Request aborted</span>`
        : ""}
    </div>
  `;
}

/** Render any in-history message; returns null for roles M1 does not display. */
export function renderHistoryMessage(
  msg: AgentMessage,
  opts: AssistantRenderOptions,
): TemplateResult | null {
  if (msg.role === "artifact") return null;
  if (msg.role === "user" || msg.role === "user-with-attachments") {
    return renderUserMessage(msg);
  }
  if (msg.role === "assistant") {
    return renderAssistantMessage(msg, opts);
  }
  // toolResult and unknown roles are not rendered standalone in M1.
  return null;
}
