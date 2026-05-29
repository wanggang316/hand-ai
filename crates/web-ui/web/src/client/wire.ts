// WebSocket wire protocol types. Every frame is one JSON object. The catalog
// is byte-compatible with the server's existing JSONL RPC protocol: command
// `type` and event `kind` are snake_case; payload fields are camelCase. Only
// the subset M0 needs is fully typed here; later milestones extend it.

import type { AgentMessage, ContentBlock, StopReason, Usage } from "../core/messages";

/**
 * Loose message shape as it appears inside agent-event frames. Message
 * lifecycle events (`message_start` / `message_end`) may carry a user message
 * whose `content` is a plain string; streaming `message_update` / `turn_end`
 * carry an assistant message whose `content` is a block array.
 */
export interface WireMessage {
  role: string;
  content: string | ContentBlock[];
  usage?: Usage;
  stopReason?: StopReason;
  model?: string;
}

// ---- client -> server -------------------------------------------------------

export interface PromptCommand {
  type: "prompt";
  id?: string;
  message: string;
  images?: string[];
}
export interface AbortCommand {
  type: "abort";
  id?: string;
}
export interface SetModelCommand {
  type: "set_model";
  id?: string;
  provider: string;
  modelId: string;
}
export interface SetThinkingLevelCommand {
  type: "set_thinking_level";
  id?: string;
  level: string;
}
export interface GetStateCommand {
  type: "get_state";
  id?: string;
}

export type ClientCommand =
  | PromptCommand
  | AbortCommand
  | SetModelCommand
  | SetThinkingLevelCommand
  | GetStateCommand;

// ---- server -> client -------------------------------------------------------

export interface ResponseFrame {
  type: "response";
  id?: string | null;
  command: string;
  success: boolean;
  data?: unknown;
  error?: string | null;
}

/** Agent-loop event payload (`kind: "agent"`), carrying a flattened event. */
export interface AgentEventPayload {
  kind: "agent";
  type: string;
  message?: WireMessage;
  messages?: AgentMessage[];
  toolCallId?: string;
  toolName?: string;
  args?: unknown;
  result?: unknown;
  isError?: boolean;
}

/** Non-agent session events (compaction, error, session-info changes). */
export interface SessionEventPayload {
  kind: Exclude<string, "agent">;
  message?: string;
  summary?: string;
  name?: string | null;
}

export type ServerEvent = AgentEventPayload | SessionEventPayload;

export interface EventFrame {
  type: "event";
  event: ServerEvent;
}

export type ServerFrame = ResponseFrame | EventFrame;

export function isAgentEvent(ev: ServerEvent): ev is AgentEventPayload {
  return ev.kind === "agent";
}
