// RemoteAgent implements the UI-facing Agent contract over a WebSocket. It
// owns the local AgentState and translates inbound server frames into the
// seven UI-facing AgentEvent variants. M0 covers the streaming text path;
// later milestones add tool-call routing, attachments, and state hydration.

import type { Agent, AgentEvent, AgentState } from "../core/agent";
import type { AssistantMessage, Attachment } from "../core/messages";
import type { Model, ThinkingLevel } from "../core/model";
import { isAgentEvent, type ServerFrame } from "./wire";
import type { WsConnection } from "./ws-connection";

export class RemoteAgent implements Agent {
  readonly state: AgentState;
  private subscribers = new Set<(event: AgentEvent) => void>();
  private nextId = 1;

  constructor(private readonly conn: WsConnection, model: Model) {
    this.state = {
      messages: [],
      model,
      thinkingLevel: "off",
      tools: [],
      pendingToolCalls: new Set<string>(),
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
      case "message_update":
        // Streaming assistant deltas. User messages are announced via
        // message_start/message_end, never here.
        if (ev.message && ev.message.role === "assistant") {
          this.emit({
            type: "message_update",
            message: ev.message as AssistantMessage,
            isStreaming: true,
          });
        }
        break;
      case "turn_end":
        // Carries the finalized assistant message for the turn.
        if (ev.message && ev.message.role === "assistant") {
          const message = ev.message as AssistantMessage;
          this.state.messages.push(message);
          this.emit({ type: "message_end", message });
        }
        this.emit({ type: "turn_end" });
        break;
      case "agent_end":
        this.state.isStreaming = false;
        if (ev.messages) this.state.messages = ev.messages;
        this.emit({ type: "agent_end", stopReason: "stop" });
        break;
      // message_start / message_end announce history additions (including the
      // user's own message, whose content is a plain string). The chat shell
      // folds those into the stable list; the M0 reply surface keys off
      // message_update + turn_end.
      default:
        break;
    }
  }
}
