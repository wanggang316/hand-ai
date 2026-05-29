// RemoteAgent implements the UI-facing Agent contract over a WebSocket. It owns
// the local AgentState (messages, model, thinkingLevel, tools, pendingToolCalls,
// isStreaming) and translates inbound server frames into the seven UI-facing
// AgentEvent variants the chat shell subscribes to.
//
// Event semantics (verified against a live turn — see the exec plan's Surprises
// section): the server announces history additions via message_start/message_end
// (which may carry the user's own message, whose content is a plain string); the
// streaming assistant content arrives via message_update; the finalized
// assistant message for the turn is carried by turn_end; the full reconciled
// list is in agent_end.messages. RemoteAgent drives the streaming container from
// message_update, folds user/assistant history additions from
// message_start/message_end (role-checked), finalizes the assistant message from
// turn_end, and reconciles the whole list on agent_end.

import type { Agent, AgentEvent, AgentState } from "../core/agent";
import type {
  AgentMessage,
  AssistantMessage,
  ContentBlock,
  Attachment,
  ToolResultContent,
  UserMessage,
} from "../core/messages";
import type { Model, ThinkingLevel } from "../core/model";
import { type AvailableModelsData, isAgentEvent, type ServerFrame, type WireMessage } from "./wire";
import type { WsConnection } from "./ws-connection";

/** Result shape a browser-executed tool returns (matches the artifacts tool). */
export interface BrowserToolResult {
  content: ToolResultContent[];
  isError?: boolean;
  details?: unknown;
}

/** A browser-side executor for a server-declared tool. */
export type BrowserToolExecutor = (
  toolCallId: string,
  args: unknown,
) => Promise<BrowserToolResult>;

// The server serializes assistant tool-call blocks with the discriminator
// "toolcall"; the rest of the UI uses the canonical "toolCall". Normalize the
// wire quirk at this boundary so renderers and core types stay canonical.
function normalizeBlock(block: ContentBlock): ContentBlock {
  if ((block as { type: string }).type === "toolcall") {
    return { ...block, type: "toolCall" } as unknown as ContentBlock;
  }
  return block;
}

function normalizeMessage(msg: AgentMessage): AgentMessage {
  if (msg.role === "assistant" && Array.isArray(msg.content)) {
    return { ...msg, content: msg.content.map(normalizeBlock) };
  }
  return msg;
}

export class RemoteAgent implements Agent {
  readonly state: AgentState;
  private subscribers = new Set<(event: AgentEvent) => void>();
  private nextId = 1;
  private pending = new Set<string>();
  // Browser-executed tools the server declares but does not run itself. Keyed
  // by tool name; invoked when a matching `tool_execution_start` arrives.
  private browserTools = new Map<string, BrowserToolExecutor>();

  constructor(
    private readonly conn: WsConnection,
    model: Model,
  ) {
    this.state = {
      messages: [],
      model,
      thinkingLevel: "off",
      tools: [],
      pendingToolCalls: this.pending,
      isStreaming: false,
    };
    this.conn.onFrame((frame) => this.handleFrame(frame));
  }

  subscribe(cb: (event: AgentEvent) => void): () => void {
    this.subscribers.add(cb);
    return () => this.subscribers.delete(cb);
  }

  /**
   * Register a browser-side executor for a server-declared tool. When the
   * server emits `tool_execution_start` for `name`, the executor runs locally
   * and its result is sent back as a `tool_result` frame keyed by toolCallId.
   */
  registerBrowserTool(name: string, execute: BrowserToolExecutor): void {
    this.browserTools.set(name, execute);
  }

  private emit(event: AgentEvent): void {
    for (const cb of this.subscribers) cb(event);
  }

  async sendMessage(text: string, _attachments?: Attachment[]): Promise<void> {
    // TODO(M10): attachment dispatch. M6 ingests attachments and shows them in
    // the composer / overlay, but does not deliver them to the agent. M10 must
    // put image content into the `prompt` frame (and have the server honor it);
    // until then `_attachments` is intentionally ignored.
    this.state.isStreaming = true;
    this.conn.send({ type: "prompt", id: String(this.nextId++), message: text });
  }

  abort(): void {
    this.conn.send({ type: "abort", id: String(this.nextId++) });
  }

  setModel(model: Model): void {
    this.state.model = model;
    this.conn.send({
      type: "set_model",
      id: String(this.nextId++),
      provider: model.provider,
      modelId: model.id,
    });
  }

  /**
   * Fetch the server's full model catalog via the correlated request/response
   * path. The server serializes each native `Model` directly, so the returned
   * objects are structurally the local `Model` type.
   */
  async getAvailableModels(): Promise<Model[]> {
    const data = await this.conn.request<AvailableModelsData>({
      type: "get_available_models",
    });
    return data?.models ?? [];
  }

  setThinkingLevel(level: ThinkingLevel): void {
    this.state.thinkingLevel = level;
    if (level !== "off") {
      this.conn.send({ type: "set_thinking_level", id: String(this.nextId++), level });
    }
  }

  /**
   * Replace the displayed transcript (and model / thinking level) with a
   * persisted session loaded from IndexedDB, then notify subscribers so the
   * chat shell re-renders the restored history.
   *
   * NB: this restores only the *browser-side* view of the conversation. The
   * server still owns its own live AgentSession; replaying the loaded history
   * into the server-side context (so the next prompt has the full history) is a
   * later concern (see M10/M12 — server-side session restore). For now the
   * displayed messages, model, and thinking level are reset and re-rendered.
   */
  loadSession(data: { messages: AgentMessage[]; model: Model; thinkingLevel: ThinkingLevel }): void {
    this.state.messages = data.messages.map(normalizeMessage);
    this.state.model = data.model;
    this.state.thinkingLevel = data.thinkingLevel;
    this.state.isStreaming = false;
    this.pending.clear();
    // Apply the restored model server-side so the next turn uses it.
    this.conn.send({
      type: "set_model",
      id: String(this.nextId++),
      provider: data.model.provider,
      modelId: data.model.id,
    });
    // agent_end is the event the chat shell uses to reconcile its view from
    // state.messages and re-enable input; reuse it to repaint the restored list.
    this.emit({ type: "agent_end", stopReason: "stop" });
  }

  /** Reset the conversation: clear local state and ask the server for a fresh session. */
  newSession(): void {
    this.state.messages = [];
    this.state.isStreaming = false;
    this.pending.clear();
    this.conn.send({ type: "new_session", id: String(this.nextId++) });
    this.emit({ type: "agent_end", stopReason: "stop" });
  }

  /** Rename the active session server-side (the browser persists titles separately). */
  setSessionName(name: string): void {
    this.conn.send({ type: "set_session_name", id: String(this.nextId++), name });
  }

  steer(message: AgentMessage): void {
    // Mid-turn steering; folds a custom message into the conversation. The wire
    // command lands with the backend-seam command catalog; until then the
    // message is appended locally so custom-message injection is observable.
    this.state.messages.push(message);
    this.emit({ type: "turn_start" });
  }

  /** Coerce a loose wire message into a typed UserMessage. */
  private toUserMessage(wire: WireMessage): UserMessage {
    return {
      role: "user",
      content: wire.content,
    };
  }

  /** Coerce a loose wire message into a typed AssistantMessage. */
  private toAssistantMessage(wire: WireMessage): AssistantMessage {
    return {
      role: "assistant",
      content: Array.isArray(wire.content) ? wire.content.map(normalizeBlock) : [],
      usage: wire.usage,
      stopReason: wire.stopReason,
      model: wire.model,
    };
  }

  private handleFrame(frame: ServerFrame): void {
    if (frame.type !== "event" || !isAgentEvent(frame.event)) return;
    const ev = frame.event;

    switch (ev.type) {
      case "agent_start":
        this.state.isStreaming = true;
        this.emit({ type: "agent_start" });
        break;

      case "turn_start":
        this.emit({ type: "turn_start" });
        break;

      case "message_start":
        // Announces a history addition. The user's own message (string content)
        // is folded into the stable list here; assistant content streams via
        // message_update. Seed the streaming container with an empty assistant
        // message so the pulsing cursor shows before the first token.
        if (ev.message?.role === "user") {
          const user = this.toUserMessage(ev.message);
          this.state.messages.push(user);
        }
        this.emit({
          type: "message_start",
          message: { role: "assistant", content: [] },
        });
        break;

      case "message_update":
        // Streaming assistant deltas.
        if (ev.message?.role === "assistant") {
          this.emit({
            type: "message_update",
            message: this.toAssistantMessage(ev.message),
            isStreaming: true,
          });
        }
        break;

      case "tool_execution_start":
        if (ev.toolCallId) this.pending.add(ev.toolCallId);
        // If this tool executes in the browser, run it locally and reply. The
        // server's tool closure is suspended until the matching tool_result
        // frame arrives, so this resolves the agent loop's pending tool call.
        if (ev.toolName && ev.toolCallId && this.browserTools.has(ev.toolName)) {
          void this.runBrowserTool(ev.toolCallId, ev.toolName, ev.args);
        }
        break;

      case "tool_execution_update":
        // Partial tool result; rendering lands with the tool-rendering milestone.
        break;

      case "tool_execution_end":
        if (ev.toolCallId) this.pending.delete(ev.toolCallId);
        break;

      case "message_end":
        // Announces a finalized history addition. The user's own message is
        // already folded in at message_start; here we clear the streaming
        // container via the UI event so the stable list owns the message.
        this.emit({
          type: "message_end",
          message: { role: "assistant", content: [] },
        });
        break;

      case "turn_end":
        // Carries the finalized assistant message for the turn.
        if (ev.message?.role === "assistant") {
          const message = this.toAssistantMessage(ev.message);
          this.state.messages.push(message);
          this.emit({ type: "message_end", message });
        }
        this.emit({ type: "turn_end" });
        break;

      case "agent_end":
        this.state.isStreaming = false;
        this.pending.clear();
        if (ev.messages) {
          this.state.messages = ev.messages.map(normalizeMessage);
        }
        this.emit({ type: "agent_end", stopReason: "stop" });
        break;

      default:
        break;
    }
  }

  /**
   * Execute a browser-side tool and send the result back to the server. The
   * server keys the reply by `toolCallId` to resume the suspended tool call.
   * Failures (thrown executor, missing registration) are reported as an error
   * tool result so the agent loop never hangs.
   */
  private async runBrowserTool(
    toolCallId: string,
    toolName: string,
    args: unknown,
  ): Promise<void> {
    const execute = this.browserTools.get(toolName);
    let content: ToolResultContent[];
    let isError: boolean;
    let details: unknown;
    if (!execute) {
      content = [{ type: "text", text: `No browser executor registered for tool ${toolName}` }];
      isError = true;
    } else {
      try {
        const result = await execute(toolCallId, args);
        content = result.content;
        isError = result.isError ?? false;
        details = result.details;
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        content = [{ type: "text", text: `Browser tool ${toolName} failed: ${message}` }];
        isError = true;
      }
    }
    this.conn.send({
      type: "tool_result",
      toolCallId,
      toolName,
      content,
      isError,
      details,
    });
  }
}
