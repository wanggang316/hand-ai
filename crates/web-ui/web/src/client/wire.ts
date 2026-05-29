// WebSocket wire protocol types. Every frame is one JSON object. The catalog
// is byte-compatible with the server's existing JSONL RPC protocol: command
// `type` and event `kind` are snake_case; payload fields are camelCase. Only
// the subset M0 needs is fully typed here; later milestones extend it.

import type { AgentMessage, ContentBlock, StopReason, Usage } from "../core/messages";
import type { Model } from "../core/model";

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

/**
 * An inline image carried in a `prompt` frame's `images` array. The field names
 * match the server's `model::types::ImageContent` wire shape (`data` +
 * `mime_type`), which is snake_case unlike the rest of the camelCase payload.
 */
export interface WireImage {
  data: string;
  mime_type: string;
}

/**
 * An out-of-band attachment reference: the content `id` returned by
 * `POST /upload` plus enough metadata for the server to fetch and label the
 * bytes. Larger files are uploaded and referenced here instead of inlined.
 */
export interface AttachmentReference {
  id: string;
  fileName: string;
  mimeType: string;
  size: number;
}

export interface PromptCommand {
  type: "prompt";
  id?: string;
  message: string;
  /** Small images inlined as base64 (server `ImageContent` shape). */
  images?: WireImage[];
  /** References to larger files uploaded out-of-band via `POST /upload`. */
  attachments?: AttachmentReference[];
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
export interface CycleModelCommand {
  type: "cycle_model";
  id?: string;
}
export interface GetAvailableModelsCommand {
  type: "get_available_models";
  id?: string;
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
export interface NewSessionCommand {
  type: "new_session";
  id?: string;
}
export interface SwitchSessionCommand {
  type: "switch_session";
  id?: string;
  sessionPath: string;
}
export interface SetSessionNameCommand {
  type: "set_session_name";
  id?: string;
  name: string;
}
/**
 * Seed the server-side session's context with a restored transcript (used when
 * loading a browser-persisted session). Only model-native roles
 * (user/assistant/toolResult) are honored; the server skips others.
 */
export interface SetMessagesCommand {
  type: "set_messages";
  id?: string;
  messages: AgentMessage[];
}
export interface GetSessionStatsCommand {
  type: "get_session_stats";
  id?: string;
}
export interface ExportHtmlCommand {
  type: "export_html";
  id?: string;
  outputPath?: string;
}
/**
 * Reply to a server-declared, browser-executed tool call (e.g. `artifacts`).
 * The server suspends the tool's execution until this frame arrives, keyed by
 * `toolCallId`. `content` carries the result blocks; the server concatenates
 * text parts into the tool result returned to the agent loop.
 */
export interface ToolResultCommand {
  type: "tool_result";
  id?: string;
  toolCallId: string;
  toolName?: string;
  content: { type: "text" | "image"; text?: string }[];
  isError: boolean;
  details?: unknown;
}

/**
 * Reply to a server-relayed extension UI request (`extension_ui_request`).
 * Mirrors the server's `RpcExtensionUiResponse` wire shape
 * (`{ type, id, ... }`), where `id` is the originating request's id (NOT a
 * fresh request/response correlation id — this frame is fire-and-forget; the
 * server resumes the suspended extension call keyed by `id`). Exactly one of
 * `value` / `confirmed` / `cancelled` is present, picked by which dialog the
 * request rendered:
 *
 * - `value` — text from a `select` / `input` / `editor` dialog.
 * - `confirmed` — boolean from a `confirm` dialog.
 * - `cancelled: true` — the user dismissed the dialog without answering.
 */
export type ExtensionUiResponseCommand = {
  type: "extension_ui_response";
  id: string;
} & ({ value: string } | { confirmed: boolean } | { cancelled: true });

export type ClientCommand =
  | PromptCommand
  | AbortCommand
  | SetModelCommand
  | CycleModelCommand
  | GetAvailableModelsCommand
  | SetThinkingLevelCommand
  | GetStateCommand
  | NewSessionCommand
  | SwitchSessionCommand
  | SetSessionNameCommand
  | SetMessagesCommand
  | GetSessionStatsCommand
  | ExportHtmlCommand
  | ToolResultCommand
  | ExtensionUiResponseCommand;

// ---- server -> client -------------------------------------------------------

export interface ResponseFrame {
  type: "response";
  id?: string | null;
  command: string;
  success: boolean;
  data?: unknown;
  error?: string | null;
}

/**
 * `get_available_models` response data. The server serializes each native
 * `Model` directly; its camelCase field set is structurally compatible with
 * the local `Model` interface (`src/core/model.ts`).
 */
export interface AvailableModelsData {
  models: Model[];
}

/** `export_html` response data: the server-side path of the written file. */
export interface ExportHtmlData {
  path: string;
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

/**
 * Server-relayed extension UI request. A distinct top-level frame `type`
 * (NOT wrapped in an `event` envelope), matching the server's
 * `RpcExtensionUiRequest` shape: `{ type: "extension_ui_request", id,
 * method, ... }`, where `method` discriminates the payload. The browser
 * renders the matching dialog and replies with an `extension_ui_response`
 * command keyed by `id`.
 *
 * NB: the server's RPC dispatcher does not currently EMIT these frames during
 * normal agent turns — they originate only when a loaded extension calls the
 * host UI (e.g. an extension's `ui.confirm(...)`). The client is wired to the
 * protocol shape so that capability lights up automatically once such an
 * extension is present; see `dialogs/extension-ui.ts`.
 */
export type ExtensionUiNotifyType = "info" | "warning" | "error";
export type ExtensionUiWidgetPlacement = "aboveEditor" | "belowEditor";

export type ExtensionUiRequestFrame = { type: "extension_ui_request"; id: string } & (
  | { method: "select"; title: string; options: string[]; timeout?: number }
  | { method: "confirm"; title: string; message: string; timeout?: number }
  | { method: "input"; title: string; placeholder?: string; timeout?: number }
  | { method: "editor"; title: string; prefill?: string }
  | { method: "notify"; message: string; notifyType?: ExtensionUiNotifyType }
  | { method: "setStatus"; statusKey: string; statusText?: string | null }
  | {
      method: "setWidget";
      widgetKey: string;
      widgetLines?: string[] | null;
      widgetPlacement?: ExtensionUiWidgetPlacement;
    }
  | { method: "setTitle"; title: string }
  | { method: "set_editor_text"; text: string }
);

export type ServerFrame = ResponseFrame | EventFrame | ExtensionUiRequestFrame;

export function isAgentEvent(ev: ServerEvent): ev is AgentEventPayload {
  return ev.kind === "agent";
}

export function isExtensionUiRequest(frame: ServerFrame): frame is ExtensionUiRequestFrame {
  return frame.type === "extension_ui_request";
}
