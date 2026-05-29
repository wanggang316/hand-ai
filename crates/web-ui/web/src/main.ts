// App bootstrap. M0 wires the WebSocket-backed RemoteAgent to a bare chat
// surface that streams one assistant reply into the page, proving the full
// browser <-> server <-> agent seam. The real Lit shell replaces this in the
// chat-shell milestone.

import "./app.css";
import { RemoteAgent } from "./client/remote-agent";
import { WsConnection } from "./client/ws-connection";
import { assistantText } from "./core/messages";
import type { Model } from "./core/model";

const wsUrl =
  (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/ws";
const conn = new WsConnection(wsUrl);

// The active model is authoritative on the server; this placeholder only
// labels the input until the model selector and get_state hydration land.
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

app.innerHTML = `
  <main style="max-width: 48rem; margin: 2rem auto; padding: 0 1rem;">
    <h1 style="font-size: 1.25rem;">hand web ui</h1>
    <form id="composer" style="display: flex; gap: 0.5rem; margin: 1rem 0;">
      <input id="prompt" style="flex: 1; padding: 0.5rem;" value="Say hello in one short sentence." />
      <button type="submit" style="padding: 0.5rem 1rem;">Send</button>
    </form>
    <pre id="reply" style="white-space: pre-wrap; border: 1px solid var(--border); padding: 1rem; min-height: 4rem;"></pre>
  </main>
`;

const replyEl = app.querySelector<HTMLPreElement>("#reply");
const formEl = app.querySelector<HTMLFormElement>("#composer");
const inputEl = app.querySelector<HTMLInputElement>("#prompt");

agent.subscribe((event) => {
  if (!replyEl) return;
  if (event.type === "message_start") replyEl.textContent = "";
  if (event.type === "message_update" || event.type === "message_end") {
    replyEl.textContent = assistantText(event.message);
  }
});

formEl?.addEventListener("submit", (e) => {
  e.preventDefault();
  const text = inputEl?.value ?? "";
  if (!text.trim()) return;
  if (replyEl) replyEl.textContent = "...";
  void agent.sendMessage(text);
});
