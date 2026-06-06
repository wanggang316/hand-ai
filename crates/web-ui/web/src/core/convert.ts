// Message conversion for the LLM transport. Mirrors the reference frontend's
// `defaultConvertToLlm` / `convertAttachments`: UI-only roles are folded into
// plain LLM messages — artifact messages are dropped, and
// user-with-attachments is expanded into a user message with image/text
// content blocks. The role guards are re-exported from core/messages for
// callers that only want the conversion module.

import {
  isArtifactMessage,
  isUserMessageWithAttachments,
  type AgentMessage,
  type Attachment,
  type ImageContent,
  type TextContent,
  type AssistantMessage,
  type ToolResultMessage,
  type UserMessage,
} from "./messages";

export { isArtifactMessage, isUserMessageWithAttachments };

/** An LLM-facing message: the subset of roles the server understands. */
export type LlmMessage = UserMessage | AssistantMessage | ToolResultMessage;

/**
 * Convert attachments to content blocks for the LLM.
 * - Images become ImageContent blocks.
 * - Documents with extracted text become a TextContent header block.
 */
export function convertAttachments(
  attachments: Attachment[],
): (TextContent | ImageContent)[] {
  const content: (TextContent | ImageContent)[] = [];
  for (const attachment of attachments) {
    if (attachment.type === "image") {
      content.push({
        type: "image",
        data: attachment.content,
        mimeType: attachment.mimeType,
      });
    } else if (attachment.type === "document" && attachment.extractedText) {
      content.push({
        type: "text",
        text: `\n\n[Document: ${attachment.fileName}]\n${attachment.extractedText}`,
      });
    }
  }
  return content;
}

/**
 * Default conversion of the UI message list to LLM messages.
 * - Filters out artifact messages (UI-only, for session reconstruction).
 * - Expands user-with-attachments into a user message with content blocks.
 * - Passes through standard user/assistant/toolResult roles.
 */
export function defaultConvertToLlm(messages: AgentMessage[]): LlmMessage[] {
  return messages
    .filter((m) => !isArtifactMessage(m))
    .map((m): LlmMessage | null => {
      if (isUserMessageWithAttachments(m)) {
        const blocks: (TextContent | ImageContent)[] =
          typeof m.content === "string"
            ? [{ type: "text", text: m.content }]
            : [...m.content];

        if (m.attachments) {
          blocks.push(...convertAttachments(m.attachments));
        }

        return {
          role: "user",
          content: blocks,
          timestamp: m.timestamp,
        };
      }

      if (m.role === "user" || m.role === "assistant" || m.role === "toolResult") {
        return m;
      }

      return null;
    })
    .filter((m): m is LlmMessage => m !== null);
}
