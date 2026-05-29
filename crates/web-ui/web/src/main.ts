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
import { i18n } from "./utils/i18n";
// Side-effect imports: register the built-in message and tool renderers.
import "./shell/messages/index";
import "./tools/index";
import "./shell/chat-panel";
import type { ChatPanel } from "./shell/chat-panel";
import "./shell/app-header";
import type { AppHeader, Theme } from "./shell/app-header";
import { ApiKeyPromptDialog } from "./dialogs/api-key-prompt-dialog";
import { ApiKeysTab } from "./dialogs/api-keys-tab";
import { ProxyTab } from "./dialogs/proxy-tab";
import { SessionListDialog } from "./dialogs/session-list-dialog";
import { SettingsDialog } from "./dialogs/settings-dialog";
import { installExtensionUiHandler, showToast } from "./dialogs/extension-ui";
import "./providers/providers-models-tab";
import type { ProvidersModelsTab } from "./providers/providers-models-tab";

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
// Reflect the server's actual active model in the UI (overrides the placeholder).
void agent.hydrate();

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
function freshSessionId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `session-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
}

// The active session id / created-at are mutable so the header's "new session"
// and "load session" actions can rebind which IndexedDB record auto-save writes.
let sessionId = freshSessionId();
let sessionCreatedAt = new Date().toISOString();

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
app.style.display = "flex";
app.style.flexDirection = "column";

// ---- App header (sessions / new / inline title / theme / settings) ----------
const header = document.createElement("app-header") as AppHeader;
header.theme = "dark";
header.title = i18n("New Session");
document.documentElement.setAttribute("data-theme", "dark");

// Restore the persisted theme asynchronously (avoid top-level await; the build
// target predates it). Basic toggle; full theming is M11.
void settingsStore
  .get<Theme>("theme")
  .then((saved) => {
    if (saved === "light" || saved === "dark") {
      header.theme = saved;
      document.documentElement.setAttribute("data-theme", saved);
    }
  })
  .catch(() => {});

const panel = document.createElement("hand-chat-panel") as ChatPanel;
// The panel must flex to fill the space below the fixed-height header.
panel.style.flex = "1";
panel.style.minHeight = "0";

app.appendChild(header);
app.appendChild(panel);

// Render server-relayed extension UI requests (select/confirm/input/editor/
// notify/...) and reply over the socket. Dormant until a loaded extension
// calls the host UI.
installExtensionUiHandler(conn, (extTitle) => {
  header.title = extTitle;
});

// Surface non-agent session events (these are not part of the agent-event
// stream RemoteAgent maps): errors as a toast, compaction status as a toast,
// and session rename into the header title.
conn.onFrame((frame) => {
  if (frame.type !== "event") return;
  const ev = frame.event as { kind?: string; message?: string; summary?: string; name?: string | null };
  switch (ev.kind) {
    case "error":
      if (ev.message) showToast(ev.message, "error");
      break;
    case "compaction_start":
      showToast(i18n("Compacting conversation..."), "info");
      break;
    case "compaction_end":
      showToast(i18n("Conversation compacted"), "info");
      break;
    case "session_info_changed":
      if (ev.name) header.title = ev.name;
      break;
    default:
      break;
  }
});

// Keep the header title in sync with the persisted session title.
async function refreshHeaderTitle(): Promise<void> {
  try {
    const meta = await sessionsStore.getMetadata(sessionId);
    if (meta?.title) header.title = meta.title;
  } catch {
    // ignore; the header keeps its current label
  }
}

header.onOpenSessions = () => {
  SessionListDialog.open(
    (id) => void loadSession(id),
    (deletedId) => {
      // If the active session was deleted, start a fresh one.
      if (deletedId === sessionId) startNewSession();
    },
  );
};

header.onNewSession = () => startNewSession();

header.onOpenSettings = () => {
  // Tab order matches the architecture: Providers & Models, Proxy, API Keys.
  const providersTab = document.createElement("providers-models-tab") as ProvidersModelsTab;
  providersTab.agent = agent;
  const proxyTab = new ProxyTab();
  const apiKeysTab = new ApiKeysTab();
  apiKeysTab.agent = agent;
  SettingsDialog.open([providersTab, proxyTab, apiKeysTab]);
};

header.onRenameTitle = (title) => {
  // Persist locally (updateTitle is a no-op until the session has been saved)
  // and inform the server so its session name matches.
  void sessionsStore.updateTitle(sessionId, title).catch((err) => {
    console.warn("rename persist failed", err);
  });
  agent.setSessionName(title);
};

header.onThemeChange = (theme) => {
  void settingsStore.set("theme", theme).catch(() => {});
};

/** Reset to a brand-new session: clear the agent and rebind the auto-save id. */
function startNewSession(): void {
  // Rebind the auto-save id BEFORE resetting the agent. agent.newSession() emits
  // agent_end synchronously, which schedules refreshHeaderTitle(); reading the
  // new (metadata-less) id there prevents it from restoring the previous
  // session's title over the "New Session" label set below.
  sessionId = freshSessionId();
  sessionCreatedAt = new Date().toISOString();
  agent.newSession();
  header.title = i18n("New Session");
  // Drop any artifacts carried over from the previous session: an empty
  // transcript reconstructs to an empty panel (collapsed, stale pill removed).
  void panel.reconstructArtifacts();
}

/**
 * Load a persisted session into the displayed conversation. Restores the
 * browser-side view (messages / model / thinking level) via RemoteAgent and
 * rebinds auto-save to that session's id so subsequent turns overwrite it.
 *
 * NB: this restores only the client-side view. Replaying the loaded transcript
 * into the server-side AgentSession context (so the next prompt carries the full
 * history) is a later concern — see M10/M12 server-side session restore.
 */
async function loadSession(id: string): Promise<void> {
  try {
    const data = await sessionsStore.get(id);
    if (!data) return;
    agent.loadSession({
      messages: data.messages,
      model: data.model,
      thinkingLevel: data.thinkingLevel,
    });
    sessionId = data.id;
    sessionCreatedAt = data.createdAt;
    header.title = data.title || i18n("New Session");
    // Replay the restored transcript's artifacts into the (collapsed) panel so
    // they are reachable again: the floating "Artifacts N" pill reappears and
    // inline "Created artifact" pills can reopen each file.
    await panel.reconstructArtifacts();
  } catch (err) {
    console.warn("session load failed", err);
  }
}

void panel.setAgent(agent, {
  // API-key gating: prompt for the provider's key when the server reports one is
  // required. Resolves true once a key is stored (or already present).
  onApiKeyRequired: async (provider: string) => {
    if (await providerKeysStore.has(provider).catch(() => false)) return true;
    return ApiKeyPromptDialog.prompt(provider);
  },
  onBeforeSend: () => {},
  onCostClick: () => {},
  // onModelSelect intentionally omitted: the chat panel's default opens the
  // model selector. (A no-op here would suppress it.)
  // The server declares the `artifacts` tool; the panel executes it in the
  // browser. Bind the executor registration to the concrete RemoteAgent here,
  // where both the agent and the panel are reachable.
  registerBrowserTool: (name, execute) => agent.registerBrowserTool(name, execute),
});

// After a turn is persisted, reflect the (possibly auto-generated) title.
agent.subscribe((event) => {
  if (event.type === "agent_end") void refreshHeaderTitle();
});
