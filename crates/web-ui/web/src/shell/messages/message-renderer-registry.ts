// Message renderer registry. Maps a message role to a renderer that turns the
// message (plus a render context carrying tool state) into a Lit template. The
// built-in renderers (user / assistant / toolResult) self-register via
// `./index`; custom roles can register their own renderer with
// `registerMessageRenderer`, mirroring the reference UI's extension point.

import type { TemplateResult } from "lit";
import type { AgentMessage, ToolResultMessage } from "../../core/messages";
import type { AgentTool } from "../../core/tool";

export type MessageRole = AgentMessage["role"];

/**
 * Context threaded into every message render. Carries the tool state needed to
 * render assistant tool-call cards in content order, and the flags that keep the
 * stable list and the streaming container from double-rendering in-flight cards.
 */
export interface MessageRenderContext {
  isStreaming: boolean;
  tools?: AgentTool[];
  pendingToolCalls?: ReadonlySet<string>;
  toolResultsById?: Map<string, ToolResultMessage>;
  /** Hide in-flight tool calls (pending, no result) to avoid duplicate cards. */
  hidePendingToolCalls?: boolean;
  /** Suppress all tool-call cards (e.g. when rendered elsewhere). */
  hideToolCalls?: boolean;
  onCostClick?: () => void;
}

export interface MessageRenderer<TMessage extends AgentMessage = AgentMessage> {
  render(message: TMessage, ctx: MessageRenderContext): TemplateResult;
}

const messageRenderers = new Map<MessageRole, MessageRenderer>();

export function registerMessageRenderer<TRole extends MessageRole>(
  role: TRole,
  renderer: MessageRenderer<Extract<AgentMessage, { role: TRole }>>,
): void {
  messageRenderers.set(role, renderer as MessageRenderer);
}

export function getMessageRenderer(role: MessageRole): MessageRenderer | undefined {
  return messageRenderers.get(role);
}

/** Render a message via its registered renderer, or undefined if none exists. */
export function renderMessage(
  message: AgentMessage,
  ctx: MessageRenderContext,
): TemplateResult | undefined {
  return messageRenderers.get(message.role)?.render(message, ctx);
}
