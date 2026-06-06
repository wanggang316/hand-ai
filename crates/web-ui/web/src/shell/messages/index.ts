// Side-effect registration of the built-in message renderers. Importing this
// module defines the message custom elements and registers a renderer for each
// built-in role (user, user-with-attachments, assistant, toolResult) into the
// message renderer registry. Each renderer maps the message + render context
// onto the corresponding element.

import { html } from "lit";
import type {
  AssistantMessage as AssistantMessageType,
  ToolResultMessage,
  UserMessage as UserMessageType,
  UserMessageWithAttachments,
} from "../../core/messages";
import { registerMessageRenderer } from "./message-renderer-registry";

import "./aborted-message";
import "./assistant-message";
import "./thinking-block";
import "./tool-message";
import "./tool-message-debug";
import "./user-message";

registerMessageRenderer("user", {
  render: (message: UserMessageType) => html`<user-message .message=${message}></user-message>`,
});

registerMessageRenderer("user-with-attachments", {
  render: (message: UserMessageWithAttachments) =>
    html`<user-message .message=${message}></user-message>`,
});

registerMessageRenderer("assistant", {
  render: (message: AssistantMessageType, ctx) =>
    html`<assistant-message
      .message=${message}
      .tools=${ctx.tools}
      .pendingToolCalls=${ctx.pendingToolCalls}
      .toolResultsById=${ctx.toolResultsById}
      .isStreaming=${ctx.isStreaming}
      .hideToolCalls=${ctx.hideToolCalls ?? false}
      .hidePendingToolCalls=${ctx.hidePendingToolCalls ?? false}
      .onCostClick=${ctx.onCostClick}
    ></assistant-message>`,
});

// toolResult messages are rendered inline within their assistant message (paired
// by toolCallId); a standalone toolResult produces no output.
registerMessageRenderer("toolResult", {
  render: (_message: ToolResultMessage) => html``,
});

export {
  getMessageRenderer,
  registerMessageRenderer,
  renderMessage,
} from "./message-renderer-registry";
export type { MessageRenderer, MessageRenderContext, MessageRole } from "./message-renderer-registry";
