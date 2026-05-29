// App bootstrap. Constructs the WebSocket-backed RemoteAgent and mounts the
// real chat shell (<hand-chat-panel>), wiring the config hooks. Storage,
// dialogs, the model selector, and the artifacts panel land in later
// milestones; their hooks are stubbed here so the contract is in place.

import "./app.css";
import { RemoteAgent } from "./client/remote-agent";
import { WsConnection } from "./client/ws-connection";
import type { Model } from "./core/model";
// Side-effect imports: register the built-in message and tool renderers.
import "./shell/messages/index";
import "./tools/index";
import "./shell/chat-panel";
import type { ChatPanel } from "./shell/chat-panel";

const wsUrl =
  (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/ws";
const conn = new WsConnection(wsUrl);

// The active model is authoritative on the server; this placeholder labels the
// input until the model selector and get_state hydration land.
const model: Model = {
  id: "deepseek/deepseek-v4-flash",
  name: "deepseek-v4-flash",
  api: "openai-completions",
  provider: "openrouter",
  reasoning: false,
  input: ["text"],
  contextWindow: 0,
  maxTokens: 0,
};

const agent = new RemoteAgent(conn, model);

const app = document.getElementById("app");
if (!app) throw new Error("missing #app mount point");

const panel = document.createElement("hand-chat-panel") as ChatPanel;
app.appendChild(panel);

void panel.setAgent(agent, {
  // Provider keys are resolved server-side; the prompt dialog lands with the
  // dialogs milestone. Returning true lets sends proceed in the meantime.
  onApiKeyRequired: async () => true,
  onBeforeSend: () => {},
  onCostClick: () => {},
  onModelSelect: () => {},
});
