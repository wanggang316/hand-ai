// Top-level chat shell layout orchestrator. Hosts the conversational
// <agent-interface> and the real <artifacts-panel>. Above the 800px breakpoint
// the two sit side-by-side; below it the artifacts panel becomes a full-screen
// overlay and a floating "Artifacts N" pill appears whenever artifacts exist and
// the panel is collapsed.
//
// The ChatPanel constructor registers the ArtifactsToolRenderer with the live
// panel ref so artifacts-tool calls render with a navigable pill. The server
// DECLARES the `artifacts` tool and the browser EXECUTES it: the panel's client
// `artifacts` tool is registered as the browser executor (via the config's
// registerBrowserTool hook) rather than added to the agent's server-side tool
// set. The pill count is driven from the panel's artifact count via
// setArtifactCount/onArtifactsChange.

import { html, LitElement } from "lit";
import { customElement, state } from "lit/decorators.js";
import type { BrowserToolResult } from "../client/remote-agent";
import { ArtifactsPanel } from "../artifacts/artifacts-panel";
import { ArtifactsToolRenderer } from "../artifacts/artifacts-tool-renderer";
import "../artifacts/index";
import type { Agent } from "../core/agent";
import type { AgentTool } from "../core/tool";
import { registerToolRenderer } from "../tools/renderer-registry";
import { Badge } from "../ui/badge";
import { i18n } from "../utils/i18n";
import "./agent-interface";
import type { AgentInterface } from "./agent-interface";

const BREAKPOINT = 800; // px — overlay vs. side-by-side

/** Config hooks forwarded into the shell at setAgent time. */
export interface ChatPanelConfig {
  onApiKeyRequired?: (provider: string) => Promise<boolean>;
  onBeforeSend?: () => void | Promise<void>;
  onCostClick?: () => void;
  onModelSelect?: () => void;
  sandboxUrlProvider?: () => string;
  toolsFactory?: (agent: Agent, agentInterface: AgentInterface) => AgentTool[];
  /**
   * Register a browser-executed tool. The server declares such tools and
   * suspends their execution until the browser replies; the panel uses this to
   * register the `artifacts` tool executor against the concrete RemoteAgent.
   */
  registerBrowserTool?: (
    name: string,
    execute: (toolCallId: string, args: unknown) => Promise<BrowserToolResult>,
  ) => void;
}

@customElement("hand-chat-panel")
export class ChatPanel extends LitElement {
  @state() private agent?: Agent;
  @state() private agentInterface?: AgentInterface;
  @state() private hasArtifacts = false;
  @state() private artifactCount = 0;
  @state() private showArtifactsPanel = false;
  @state() private windowWidth = 0;

  // The live artifacts panel; created in setAgent.
  private artifactsPanel: ArtifactsPanel;
  private sandboxUrlProvider?: () => string;
  // True while reconstructFromMessages is replaying history; suppresses the
  // auto-open / count-bump that a real user-driven create would trigger.
  private reconstructing = false;

  constructor() {
    super();
    // Construct the panel and register its tool renderer up front so the
    // renderer (with the live panel ref) is available before the first render.
    this.artifactsPanel = new ArtifactsPanel();
    registerToolRenderer("artifacts", new ArtifactsToolRenderer(this.artifactsPanel));
  }

  private resizeHandler = () => {
    this.windowWidth = window.innerWidth;
    this.requestUpdate();
  };

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.windowWidth = window.innerWidth;
    window.addEventListener("resize", this.resizeHandler);
    this.style.display = "flex";
    this.style.flexDirection = "column";
    this.style.height = "100%";
    this.style.minHeight = "0";
    requestAnimationFrame(() => {
      this.windowWidth = window.innerWidth;
      this.requestUpdate();
    });
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    window.removeEventListener("resize", this.resizeHandler);
  }

  async setAgent(agent: Agent, config?: ChatPanelConfig): Promise<void> {
    this.agent = agent;
    this.sandboxUrlProvider = config?.sandboxUrlProvider;

    const agentInterface = document.createElement("agent-interface") as AgentInterface;
    agentInterface.session = agent;
    agentInterface.enableAttachments = true;
    agentInterface.enableModelSelector = true;
    agentInterface.enableThinkingSelector = true;
    agentInterface.showThemeToggle = false;
    agentInterface.onApiKeyRequired = config?.onApiKeyRequired;
    agentInterface.onBeforeSend = config?.onBeforeSend;
    agentInterface.onCostClick = config?.onCostClick;
    agentInterface.onModelSelect = config?.onModelSelect;
    this.agentInterface = agentInterface;

    // Wire the artifacts panel to the agent + sandbox CSP provider + callbacks.
    this.artifactsPanel.agent = agent;
    if (this.sandboxUrlProvider) this.artifactsPanel.sandboxUrlProvider = this.sandboxUrlProvider;
    this.artifactsPanel.onArtifactsChange = () => this.handleArtifactsChange();
    this.artifactsPanel.onOpen = () => {
      this.showArtifactsPanel = true;
      this.requestUpdate();
    };
    this.artifactsPanel.onClose = () => {
      this.showArtifactsPanel = false;
      this.requestUpdate();
    };

    // The SERVER declares the `artifacts` tool; the browser only EXECUTES it.
    // Register the panel's client tool as the browser executor so a server
    // `tool_execution_start` for "artifacts" runs locally and replies. The
    // panel's native tool signature is execute(toolCallId, args, signal).
    const panelTool = this.artifactsPanel.tool;
    config?.registerBrowserTool?.(
      panelTool.name,
      async (toolCallId: string, args: unknown): Promise<BrowserToolResult> => {
        const result = await panelTool.execute(
          toolCallId,
          args as Parameters<typeof panelTool.execute>[1],
        );
        return { content: result.content, isError: false };
      },
    );

    const additionalTools = config?.toolsFactory?.(agent, agentInterface) ?? [];
    if (additionalTools.length > 0) {
      agent.state.tools = [...agent.state.tools, ...additionalTools];
    }

    // Replay any artifact history already present (e.g. restored session)
    // without auto-opening the panel — preserve the null-during-reconstruct
    // ordering by guarding the change handler while reconstructing.
    await this.reconstructArtifacts();

    this.requestUpdate();
  }

  /** Replay artifact history from the current message list (no auto-open). */
  public async reconstructArtifacts(): Promise<void> {
    if (!this.agent) return;
    this.reconstructing = true;
    try {
      await this.artifactsPanel.reconstructFromMessages(this.agent.state.messages);
    } finally {
      this.reconstructing = false;
    }
    // Sync the count after reconstruction without forcing the panel open.
    const count = this.artifactsPanel.artifacts.size;
    this.hasArtifacts = count > 0;
    this.artifactCount = count;
    this.requestUpdate();
  }

  /** Called by the panel whenever its artifact set changes. */
  private handleArtifactsChange(): void {
    const count = this.artifactsPanel.artifacts.size;
    if (this.reconstructing) {
      // During reconstruction, update count only; never auto-open.
      this.hasArtifacts = count > 0;
      this.artifactCount = count;
      this.requestUpdate();
      return;
    }
    this.setArtifactCount(count);
  }

  /** The sandbox CSP URL provider, consumed by the artifacts panel. */
  public getSandboxUrlProvider(): (() => string) | undefined {
    return this.sandboxUrlProvider;
  }

  /**
   * Set the artifact count. A net-new artifact auto-opens the panel; the pill /
   * overlay logic keys off hasArtifacts + showArtifactsPanel.
   */
  public setArtifactCount(count: number): void {
    const created = count > this.artifactCount;
    this.hasArtifacts = count > 0;
    this.artifactCount = count;
    if (this.hasArtifacts && created) {
      this.showArtifactsPanel = true;
    }
    this.requestUpdate();
  }

  override render() {
    if (!this.agent || !this.agentInterface) {
      return html`<div class="flex items-center justify-center h-full">
        <div class="text-muted-foreground">${i18n("No agent set")}</div>
      </div>`;
    }

    const isMobile = this.windowWidth < BREAKPOINT;
    const showPanel = this.showArtifactsPanel && this.hasArtifacts;

    // Keep the panel's collapsed/overlay flags in sync with the layout.
    this.artifactsPanel.collapsed = !showPanel;
    this.artifactsPanel.overlay = isMobile && showPanel;

    return html`
      <div class="relative w-full h-full overflow-hidden flex">
        <!-- Chat column -->
        <div class="h-full" style=${!isMobile && showPanel ? "width: 50%;" : "width: 100%;"}>
          ${this.agentInterface}
        </div>

        <!-- Floating pill: artifacts exist but panel collapsed -->
        ${this.hasArtifacts && !this.showArtifactsPanel
          ? html`<button
              class="absolute z-30 top-4 left-1/2 -translate-x-1/2 pointer-events-auto"
              title=${i18n("Show artifacts")}
              @click=${() => {
                this.showArtifactsPanel = true;
                this.requestUpdate();
              }}
            >
              ${Badge(html`<span class="inline-flex items-center gap-1">
                <span>${i18n("Artifacts")}</span>
                <span
                  class="text-[10px] leading-none bg-primary-foreground/20 rounded px-1 font-mono tabular-nums"
                  >${this.artifactCount}</span
                >
              </span>`)}
            </button>`
          : ""}

        <!-- Artifacts column / overlay -->
        <div
          class="h-full ${isMobile ? "absolute inset-0 pointer-events-auto" : ""}"
          style=${isMobile
            ? showPanel
              ? ""
              : "display: none;"
            : showPanel
              ? "width: 50%;"
              : "display: none;"}
        >
          ${this.artifactsPanel}
        </div>
      </div>
    `;
  }
}
