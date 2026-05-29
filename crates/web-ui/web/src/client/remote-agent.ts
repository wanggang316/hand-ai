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
  UserMessage,
} from "../core/messages";
import type { Model, ThinkingLevel } from "../core/model";
import { isAgentEvent, type ServerFrame, type WireMessage } from "./wire";
import type { WsConnection } from "./ws-connection";

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

  private emit(event: AgentEvent): void {
    for (const cb of this.subscribers) cb(event);
  }

  async sendMessage(text: string, _attachments?: Attachment[]): Promise<void> {
    // Attachment dispatch (inline base64 vs. upload reference) lands with the
    // networking milestone; M1 sends text prompts.
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

  setThinkingLevel(level: ThinkingLevel): void {
    this.state.thinkingLevel = level;
    if (level !== "off") {
      this.conn.send({ type: "set_thinking_level", id: String(this.nextId++), level });
    }
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
}
