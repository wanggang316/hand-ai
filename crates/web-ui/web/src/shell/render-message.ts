// Message rendering entry point for the chat shell. Delegates to the message
// renderer registry (and, transitively, the tool renderer registry) so message
// and tool-call rendering is fully driven by the registries. The exported
// function names/signatures are kept stable so the M1 shell components
// (message-list, streaming-message-container) compile and behave unchanged.
//
// Importing this module self-registers the built-in message and tool renderers.

import { html, type TemplateResult } from "lit";
import type {
  AgentMessage,
  AssistantMessage,
  ToolResultMessage,
  UserMessage,
  UserMessageWithAttachments,
} from "../core/messages";
import type { AgentTool } from "../core/tool";
import "../tools/index";
import {
  renderMessage,
  type MessageRenderContext,
} from "./messages/index";

export interface AssistantRenderOptions {
  isStreaming: boolean;
  /** Tools available for resolving tool-call renderers and names. */
  tools?: AgentTool[];
  /** Tool-call ids currently executing (no result yet). */
  pendingToolCalls?: ReadonlySet<string>;
  /** Tool results keyed by their tool-call id, for pairing. */
  toolResultsById?: Map<string, ToolResultMessage>;
  /**
   * When true, in-flight tool calls (pending, no result) are hidden so the
   * streaming container and the stable list never render the same card twice.
   */
  hidePendingToolCalls?: boolean;
  onCostClick?: () => void;
}

function toContext(opts: AssistantRenderOptions): MessageRenderContext {
  return {
    isStreaming: opts.isStreaming,
    tools: opts.tools,
    pendingToolCalls: opts.pendingToolCalls,
    toolResultsById: opts.toolResultsById,
    hidePendingToolCalls: opts.hidePendingToolCalls,
    onCostClick: opts.onCostClick,
  };
}

export function renderUserMessage(
  msg: UserMessage | UserMessageWithAttachments,
): TemplateResult {
  return renderMessage(msg, { isStreaming: false }) ?? html``;
}

/** Render an assistant message in content order via the registry. */
export function renderAssistantMessage(
  msg: AssistantMessage,
  opts: AssistantRenderOptions,
): TemplateResult {
  return renderMessage(msg, toContext(opts)) ?? html``;
}

/** Render any in-history message; returns null for roles with no renderer/output. */
export function renderHistoryMessage(
  msg: AgentMessage,
  opts: AssistantRenderOptions,
): TemplateResult | null {
  if (msg.role === "artifact") return null;
  // toolResult is paired into its assistant message; the registered renderer
  // produces empty output, so suppress it from the standalone history list.
  if (msg.role === "toolResult") return null;
  return renderMessage(msg, toContext(opts)) ?? null;
}
