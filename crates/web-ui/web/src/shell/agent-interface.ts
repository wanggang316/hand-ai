// Conversational shell. A view over an Agent (the RemoteAgent in this app):
// subscribes to its event stream, renders the stable message list plus the live
// streaming container, a per-turn cost stats bar, an abort affordance, and the
// bottom-anchored editor. Auto-scroll follows the reference behavior: a
// ResizeObserver on the content keeps the view pinned to the bottom; a scroll
// listener disables auto-scroll when the user scrolls up and re-enables it near
// the bottom; a clientHeight-shrink guard prevents the stats bar appearing (which
// shrinks the scroll container) from false-disabling auto-scroll.

import { html, LitElement } from "lit";
import { customElement, property, query } from "lit/decorators.js";
import type { Agent, AgentEvent } from "../core/agent";
import type { Attachment, ToolResultMessage, Usage } from "../core/messages";
import type { ThinkingLevel } from "../core/model";
import { formatUsage } from "../utils/format";
import { i18n } from "../utils/i18n";
import "./message-editor";
import type { MessageEditor } from "./message-editor";
import "./message-list";
import "./streaming-message-container";
import type { StreamingMessageContainer } from "./streaming-message-container";

@customElement("agent-interface")
export class AgentInterface extends LitElement {
  // External session: when provided, this component is a view over it.
  @property({ attribute: false }) session?: Agent;
  @property({ type: Boolean }) enableAttachments = true;
  @property({ type: Boolean }) enableModelSelector = true;
  @property({ type: Boolean }) enableThinkingSelector = true;
  @property({ type: Boolean }) showThemeToggle = false;
  @property({ attribute: false }) onApiKeyRequired?: (provider: string) => Promise<boolean>;
  @property({ attribute: false }) onBeforeSend?: () => void | Promise<void>;
  @property({ attribute: false }) onCostClick?: () => void;
  @property({ attribute: false }) onModelSelect?: () => void;

  @query("message-editor") private _messageEditor!: MessageEditor;
  @query("streaming-message-container") private _streamingContainer!: StreamingMessageContainer;

  private _autoScroll = true;
  private _lastScrollTop = 0;
  private _lastClientHeight = 0;
  private _scrollContainer?: HTMLElement;
  private _resizeObserver?: ResizeObserver;
  private _unsubscribeSession?: () => void;
  private _unsubscribeConnection?: () => void;

  // Live transport status (transport-backed agents only). Defaults to connected
  // so in-memory agents — which expose no isConnected — never gate or annotate.
  // `_wasConnected` distinguishes the first connect ("Connecting…") from a drop
  // after a successful connect ("Reconnecting…").
  private _connected = true;
  private _wasConnected = false;

  /** Set the editor's text (and optional attachments). */
  public setInput(text: string, attachments?: Attachment[]): void {
    const update = () => {
      if (!this._messageEditor) {
        requestAnimationFrame(update);
      } else {
        this._messageEditor.value = text;
        this._messageEditor.attachments = attachments ?? [];
      }
    };
    update();
  }

  public setAutoScroll(enabled: boolean): void {
    this._autoScroll = enabled;
  }

  /** Replace the active session and resubscribe. */
  public setAgent(agent: Agent): void {
    this.session = agent;
  }

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override willUpdate(changed: Map<string, unknown>): void {
    super.willUpdate(changed);
    if (changed.has("session")) {
      this.setupSessionSubscription();
    }
  }

  override async connectedCallback(): Promise<void> {
    super.connectedCallback();

    this.style.display = "flex";
    this.style.flexDirection = "column";
    this.style.height = "100%";
    this.style.minHeight = "0";

    await this.updateComplete;
    this._scrollContainer = this.querySelector(".overflow-y-auto") as HTMLElement;

    if (this._scrollContainer) {
      this._resizeObserver = new ResizeObserver(() => {
        if (this._autoScroll && this._scrollContainer) {
          this._scrollContainer.scrollTop = this._scrollContainer.scrollHeight;
        }
      });
      const contentContainer = this._scrollContainer.querySelector(".max-w-3xl");
      if (contentContainer) {
        this._resizeObserver.observe(contentContainer);
      }
      this._scrollContainer.addEventListener("scroll", this._handleScroll);
    }

    this.setupSessionSubscription();
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    if (this._resizeObserver) {
      this._resizeObserver.disconnect();
      this._resizeObserver = undefined;
    }
    if (this._scrollContainer) {
      this._scrollContainer.removeEventListener("scroll", this._handleScroll);
    }
    if (this._unsubscribeSession) {
      this._unsubscribeSession();
      this._unsubscribeSession = undefined;
    }
    if (this._unsubscribeConnection) {
      this._unsubscribeConnection();
      this._unsubscribeConnection = undefined;
    }
  }

  private setupSessionSubscription(): void {
    if (this._unsubscribeSession) {
      this._unsubscribeSession();
      this._unsubscribeSession = undefined;
    }
    if (this._unsubscribeConnection) {
      this._unsubscribeConnection();
      this._unsubscribeConnection = undefined;
    }
    if (!this.session) return;

    // Track transport connection status so the editor can reflect "Connecting…/
    // Reconnecting…" and hold back sends instead of dropping them silently. Only
    // transport-backed agents expose isConnected; others stay always-connected.
    if (this.session.isConnected) {
      this._connected = this.session.isConnected();
      if (this._connected) this._wasConnected = true;
    }
    if (this.session.onConnectionChange) {
      this._unsubscribeConnection = this.session.onConnectionChange((connected) => {
        this._connected = connected;
        if (connected) this._wasConnected = true;
        this.requestUpdate();
      });
    }

    this._unsubscribeSession = this.session.subscribe((ev: AgentEvent) => {
      switch (ev.type) {
        case "message_start":
        case "turn_start":
        case "turn_end":
        case "agent_start":
          this.requestUpdate();
          break;
        case "message_end":
          // Clear the streaming container; the stable list now owns this message.
          if (this._streamingContainer) {
            this._streamingContainer.setMessage(null, true);
          }
          this.requestUpdate();
          break;
        case "agent_end":
          if (this._streamingContainer) {
            this._streamingContainer.isStreaming = false;
            this._streamingContainer.setMessage(null, true);
          }
          this.requestUpdate();
          break;
        case "message_update":
          if (this._streamingContainer) {
            const isStreaming = this.session?.state.isStreaming ?? false;
            this._streamingContainer.isStreaming = isStreaming;
            this._streamingContainer.setMessage(ev.message, !isStreaming);
          }
          this.requestUpdate();
          break;
      }
    });
  }

  private _handleScroll = (): void => {
    if (!this._scrollContainer) return;

    const currentScrollTop = this._scrollContainer.scrollTop;
    const scrollHeight = this._scrollContainer.scrollHeight;
    const clientHeight = this._scrollContainer.clientHeight;
    const distanceFromBottom = scrollHeight - currentScrollTop - clientHeight;

    // Ignore relayout from the editor being pushed up by the stats bar
    // appearing (which shrinks clientHeight). Without this guard the resulting
    // scroll event would false-disable auto-scroll.
    if (clientHeight < this._lastClientHeight) {
      this._lastClientHeight = clientHeight;
      return;
    }

    if (currentScrollTop !== 0 && currentScrollTop < this._lastScrollTop && distanceFromBottom > 50) {
      this._autoScroll = false;
    } else if (distanceFromBottom < 10) {
      this._autoScroll = true;
    }

    this._lastScrollTop = currentScrollTop;
    this._lastClientHeight = clientHeight;
  };

  /** Send a message through the active session, honoring the config hooks. */
  public async sendMessage(input: string, attachments?: Attachment[]): Promise<void> {
    const session = this.session;
    if (!session) throw new Error("No session set on AgentInterface");
    if ((!input.trim() && (attachments?.length ?? 0) === 0) || session.state.isStreaming) {
      return;
    }
    if (!session.state.model) throw new Error("No model set on AgentInterface");

    // Optional API-key gating. Keys are resolved server-side in this app, so
    // gating only runs when the session exposes getApiKey and reports none.
    if (session.getApiKey) {
      const provider = session.state.model.provider;
      const key = await session.getApiKey(provider);
      if (!key && this.onApiKeyRequired) {
        const ok = await this.onApiKeyRequired(provider);
        if (!ok) return;
      }
    }

    if (this.onBeforeSend) {
      await this.onBeforeSend();
    }

    // Only clear the editor once we know we can send.
    this._messageEditor.value = "";
    this._messageEditor.attachments = [];
    this._autoScroll = true;

    await session.sendMessage(input, attachments);
  }

  private renderMessages() {
    if (!this.session) {
      return html`<div class="p-4 text-center text-muted-foreground">${i18n("No session available")}</div>`;
    }
    const state = this.session.state;
    const toolResultsById = new Map<string, ToolResultMessage>();
    for (const message of state.messages) {
      if (message.role === "toolResult") {
        toolResultsById.set(message.toolCallId, message);
      }
    }
    return html`
      <div class="flex flex-col gap-3">
        <message-list
          .messages=${state.messages}
          .tools=${state.tools}
          .pendingToolCalls=${state.pendingToolCalls}
          .isStreaming=${state.isStreaming}
          .onCostClick=${this.onCostClick}
        ></message-list>

        <streaming-message-container
          class=${state.isStreaming ? "" : "hidden"}
          .tools=${state.tools}
          .isStreaming=${state.isStreaming}
          .pendingToolCalls=${state.pendingToolCalls}
          .toolResultsById=${toolResultsById}
          .onCostClick=${this.onCostClick}
        ></streaming-message-container>
      </div>
    `;
  }

  private renderStats() {
    if (!this.session) return html`<div class="text-xs h-5"></div>`;

    const totals: Usage = this.session.state.messages
      .filter((m) => m.role === "assistant")
      .reduce<Usage>(
        (acc, msg) => {
          const usage = (msg as { usage?: Usage }).usage;
          if (usage) {
            acc.input += usage.input;
            acc.output += usage.output;
            acc.cacheRead += usage.cacheRead;
            acc.cacheWrite += usage.cacheWrite;
            acc.totalTokens += usage.totalTokens;
            acc.cost.total += usage.cost.total;
          }
          return acc;
        },
        {
          input: 0,
          output: 0,
          cacheRead: 0,
          cacheWrite: 0,
          totalTokens: 0,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
        },
      );

    const hasTotals = totals.input || totals.output || totals.cacheRead || totals.cacheWrite;
    const totalsText = hasTotals ? formatUsage(totals) : "";

    return html`
      <div class="text-xs text-muted-foreground flex justify-between items-center h-5">
        <div class="flex items-center gap-1"></div>
        <div class="flex ml-auto items-center gap-3">
          ${totalsText
            ? this.onCostClick
              ? html`<span
                  class="cursor-pointer hover:text-foreground transition-colors"
                  @click=${this.onCostClick}
                  >${totalsText}</span
                >`
              : html`<span>${totalsText}</span>`
            : ""}
        </div>
      </div>
    `;
  }

  override render() {
    if (!this.session) {
      return html`<div class="p-4 text-center text-muted-foreground">${i18n("No session set")}</div>`;
    }

    const session = this.session;
    const state = session.state;
    return html`
      <div class="flex flex-col h-full bg-background text-foreground">
        <!-- Messages -->
        <div class="flex-1 overflow-y-auto">
          <div class="max-w-3xl mx-auto p-4 pb-0">${this.renderMessages()}</div>
        </div>

        <!-- Input -->
        <div class="shrink-0">
          <div class="max-w-3xl mx-auto px-2">
            ${session.isConnected && !this._connected
              ? html`<div
                  class="flex items-center gap-1.5 px-1 pb-1 text-xs text-amber-600 dark:text-amber-400"
                  role="status"
                >
                  <span
                    class="inline-block w-1.5 h-1.5 rounded-full bg-amber-500 animate-pulse"
                  ></span>
                  <span>${this._wasConnected ? i18n("Reconnecting…") : i18n("Connecting…")}</span>
                </div>`
              : ""}
            <message-editor
              .isStreaming=${state.isStreaming}
              .currentModel=${state.model}
              .thinkingLevel=${state.thinkingLevel}
              .showAttachmentButton=${this.enableAttachments}
              .showModelSelector=${this.enableModelSelector}
              .showThinkingSelector=${this.enableThinkingSelector}
              .onSend=${(input: string, attachments: Attachment[]) => {
                void this.sendMessage(input, attachments);
              }}
              .onAbort=${() => session.abort()}
              .onModelSelect=${() => this.onModelSelect?.()}
              .onThinkingChange=${
                this.enableThinkingSelector
                  ? (level: ThinkingLevel) => session.setThinkingLevel(level)
                  : undefined
              }
            ></message-editor>
            ${this.renderStats()}
          </div>
        </div>
      </div>
    `;
  }
}
