// The UI-facing agent contract. Every chat view subscribes to an object that
// implements this interface; in the web app the only implementation is the
// RemoteAgent, which proxies a WebSocket to the Rust server and re-emits the
// same event stream the reference in-browser agent produced.

import type { AgentMessage, AssistantMessage, Attachment } from "./messages";
import type { Model, ThinkingLevel } from "./model";
import type { AgentTool } from "./tool";

export type AgentEvent =
  | { type: "agent_start" }
  | { type: "turn_start" }
  | { type: "message_start"; message: AssistantMessage }
  | { type: "message_update"; message: AssistantMessage; isStreaming: boolean }
  | { type: "message_end"; message: AssistantMessage }
  | { type: "turn_end" }
  | { type: "agent_end"; stopReason: string }
  // Fired when the active model or thinking level changes outside a turn (e.g.
  // the model selector). Lets views re-render the composer immediately rather
  // than only on the next turn event.
  | { type: "model_change" };

export interface AgentState {
  messages: AgentMessage[];
  model: Model;
  thinkingLevel: ThinkingLevel;
  tools: AgentTool[];
  pendingToolCalls: ReadonlySet<string>;
  isStreaming: boolean;
}

export interface Agent {
  readonly state: AgentState;
  subscribe(cb: (event: AgentEvent) => void): () => void;
  sendMessage(text: string, attachments?: Attachment[]): Promise<void>;
  abort(): void;
  setModel(model: Model): void;
  setThinkingLevel(level: ThinkingLevel): void;
  getApiKey?(provider: string): Promise<string | undefined>;
  steer?(message: AgentMessage): void;
  /**
   * Transport-backed agents (e.g. the WebSocket-backed RemoteAgent) report
   * whether the connection is currently usable. Views use this to avoid
   * dispatching a send into a not-yet-open / dropped socket. Absent on in-memory
   * agents, where the view treats the agent as always connected.
   */
  isConnected?(): boolean;
  /** Subscribe to connection up/down transitions; returns an unsubscribe fn. */
  onConnectionChange?(cb: (connected: boolean) => void): () => void;
}
