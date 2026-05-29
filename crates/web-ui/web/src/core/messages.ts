// Local message types. A de-branded, structurally-compatible mirror of the
// agent message model the Rust server serializes. UI-only roles
// (user-with-attachments, artifact) are layered on via the
// CustomAgentMessages extension point so consumer code can add roles.

export interface TextContent {
  type: "text";
  text: string;
}

export interface ThinkingContent {
  type: "thinking";
  thinking: string;
}

export interface ImageContent {
  type: "image";
  data: string;
  mimeType: string;
}

export interface ToolCall {
  type: "toolCall";
  id: string;
  name: string;
  arguments: unknown;
}

export type ContentBlock = TextContent | ThinkingContent | ImageContent | ToolCall;

export interface UsageCost {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  total: number;
}

export interface Usage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  totalTokens: number;
  cost: UsageCost;
}

export type StopReason = "stop" | "aborted" | "error" | "toolUse";

export interface UserMessage {
  role: "user";
  content: string | ContentBlock[];
  timestamp?: number;
}

export interface AssistantMessage {
  role: "assistant";
  content: ContentBlock[];
  usage?: Usage;
  stopReason?: StopReason;
  model?: string;
  errorMessage?: string;
}

export interface ToolResultContent {
  type: "text" | "image";
  text?: string;
}

export interface ToolResultMessage<D = unknown> {
  role: "toolResult";
  toolCallId: string;
  content: ToolResultContent[];
  isError: boolean;
  details?: D;
}

/** Minimal attachment shape; the full ingestion model lands with attachments. */
export interface Attachment {
  id: string;
  type: "image" | "document";
  fileName: string;
  mimeType: string;
  size: number;
  /** base64-encoded bytes. */
  content: string;
  extractedText?: string;
  /** base64 preview image. */
  preview?: string;
}

export interface UserMessageWithAttachments {
  role: "user-with-attachments";
  content: string | (TextContent | ImageContent)[];
  timestamp: number;
  attachments?: Attachment[];
}

export interface ArtifactMessage {
  role: "artifact";
  action: "create" | "update" | "delete";
  filename: string;
  content?: string;
  title?: string;
  timestamp: string;
}

// Declaration-merging extension point: consumers add custom roles by
// augmenting this interface.
export interface CustomAgentMessages {
  "user-with-attachments": UserMessageWithAttachments;
  artifact: ArtifactMessage;
}

export type AgentMessage =
  | UserMessage
  | AssistantMessage
  | ToolResultMessage
  | CustomAgentMessages[keyof CustomAgentMessages];

export type MessageRole = AgentMessage["role"];

export function isUserMessageWithAttachments(
  msg: AgentMessage,
): msg is UserMessageWithAttachments {
  return msg.role === "user-with-attachments";
}

export function isArtifactMessage(msg: AgentMessage): msg is ArtifactMessage {
  return msg.role === "artifact";
}

/**
 * Concatenate the text of a message. Message lifecycle events carry either a
 * user message (string content) or an assistant message (content blocks), so
 * both shapes are handled.
 */
export function assistantText(msg: { content: string | ContentBlock[] }): string {
  if (typeof msg.content === "string") return msg.content;
  return msg.content
    .filter((b): b is TextContent => b.type === "text")
    .map((b) => b.text)
    .join("");
}
