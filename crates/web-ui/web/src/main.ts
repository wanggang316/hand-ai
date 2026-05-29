// App bootstrap. Constructs the WebSocket-backed RemoteAgent and mounts the
// real chat shell (<hand-chat-panel>), wiring the config hooks. Storage,
// dialogs, the model selector, and the artifacts panel land in later
// milestones; their hooks are stubbed here so the contract is in place.

import "./app.css";
import { RemoteAgent } from "./client/remote-agent";
import { WsConnection } from "./client/ws-connection";
import type { AgentMessage } from "./core/messages";
import type { Model } from "./core/model";
import {
  AppStorage,
  buildStorageConfig,
  setAppStorage,
} from "./storage/app-storage";
import type { SessionData, SessionMetadata } from "./storage/backend";
import { CustomProvidersStore } from "./storage/custom-providers-store";
import { IndexedDBStorageBackend } from "./storage/indexeddb-backend";
import { ProviderKeysStore } from "./storage/provider-keys-store";
import { SessionsStore } from "./storage/sessions-store";
import { SettingsStore } from "./storage/settings-store";
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

// ---- IndexedDB persistence (db name "hand-ai") ------------------------------
// Construct the four stores + a versioned IndexedDB backend, wire the backend
// into each store, and register the singleton. The sessions store contributes
// both its primary and companion metadata-store schema via buildStorageConfig.
const settingsStore = new SettingsStore();
const providerKeysStore = new ProviderKeysStore();
const sessionsStore = new SessionsStore();
const customProvidersStore = new CustomProvidersStore();

const storageBackend = new IndexedDBStorageBackend(
  buildStorageConfig("hand-ai", {
    settings: settingsStore,
    providerKeys: providerKeysStore,
    sessions: sessionsStore,
    customProviders: customProvidersStore,
  }),
);

for (const store of [
  settingsStore,
  providerKeysStore,
  sessionsStore,
  customProvidersStore,
]) {
  store.setBackend(storageBackend);
}

setAppStorage(
  new AppStorage(
    settingsStore,
    providerKeysStore,
    sessionsStore,
    customProvidersStore,
    storageBackend,
  ),
);

// Auto-save: persist the live transcript + model + thinking level to the
// SessionsStore whenever the agent finishes a turn (agent_end) or commits a
// message to history (message_end). A stable per-tab session id is generated
// once so successive turns overwrite the same record. Persistence is fully
// resilient: any IndexedDB failure is swallowed so storage errors never crash
// the chat. (Loading a persisted session and choosing the id from existing
// metadata is wired by the sessions dialog in a later milestone; until then a
// fresh id is minted per page load.)
const sessionId =
  typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `session-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
const sessionCreatedAt = new Date().toISOString();

function buildPreview(messages: AgentMessage[]): string {
  const parts: string[] = [];
  for (const msg of messages) {
    if (msg.role !== "user" && msg.role !== "assistant") continue;
    const text =
      typeof msg.content === "string"
        ? msg.content
        : msg.content
            .filter((b): b is { type: "text"; text: string } => b.type === "text")
            .map((b) => b.text)
            .join("");
    if (text) parts.push(text);
    if (parts.join("\n").length >= 2048) break;
  }
  return parts.join("\n").slice(0, 2048);
}

async function persistSession(): Promise<void> {
  const { messages, model: activeModel, thinkingLevel } = agent.state;
  if (messages.length === 0) return;
  const now = new Date().toISOString();
  const title =
    (await sessionsStore.getMetadata(sessionId))?.title || buildPreview(messages).slice(0, 80);
  const data: SessionData = {
    id: sessionId,
    title,
    model: activeModel,
    thinkingLevel,
    messages,
    createdAt: sessionCreatedAt,
    lastModified: now,
  };
  const metadata: SessionMetadata = {
    id: sessionId,
    title,
    createdAt: sessionCreatedAt,
    lastModified: now,
    messageCount: messages.length,
    usage: {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
    thinkingLevel,
    preview: buildPreview(messages),
  };
  await sessionsStore.save(data, metadata);
}

agent.subscribe((event) => {
  if (event.type === "agent_end" || event.type === "message_end") {
    persistSession().catch((err) => {
      console.warn("session auto-save failed", err);
    });
  }
});

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
  // onModelSelect intentionally omitted: the chat panel's default opens the
  // model selector. (A no-op here would suppress it.)
  // The server declares the `artifacts` tool; the panel executes it in the
  // browser. Bind the executor registration to the concrete RemoteAgent here,
  // where both the agent and the panel are reachable.
  registerBrowserTool: (name, execute) => agent.registerBrowserTool(name, execute),
});
