// Top-level chat shell layout orchestrator. Hosts the conversational
// <agent-interface> and an artifacts panel slot. Above the 800px breakpoint the
// two sit side-by-side; below it the artifacts panel becomes a full-screen
// overlay and a floating "Artifacts N" pill appears whenever artifacts exist and
// the panel is collapsed.
//
// In M1 the artifacts panel is a STUB (an empty bordered placeholder; the real
// panel lands in the artifacts milestone). The artifact count is held at 0 here,
// but the pill / overlay / breakpoint / show-hide logic is fully implemented so
// it works as soon as artifacts exist.

import { html, LitElement } from "lit";
import { customElement, state } from "lit/decorators.js";
import type { Agent } from "../core/agent";
import type { AgentTool } from "../core/tool";
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
}

@customElement("hand-chat-panel")
export class ChatPanel extends LitElement {
  @state() private agent?: Agent;
  @state() private agentInterface?: AgentInterface;
  @state() private hasArtifacts = false;
  @state() private artifactCount = 0;
  @state() private showArtifactsPanel = false;
  @state() private windowWidth = 0;

  // Forwarded for the artifacts panel once it lands (sandbox CSP delivery).
  private sandboxUrlProvider?: () => string;

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

    // The artifacts tool factory lands with the artifacts milestone. M1 forwards
    // any consumer-provided tools onto the agent state so the contract is real.
    const additionalTools = config?.toolsFactory?.(agent, agentInterface) ?? [];
    if (additionalTools.length > 0) {
      agent.state.tools = [...agent.state.tools, ...additionalTools];
    }

    this.requestUpdate();
  }

  /** The sandbox CSP URL provider, consumed by the artifacts panel once it lands. */
  public getSandboxUrlProvider(): (() => string) | undefined {
    return this.sandboxUrlProvider;
  }

  /**
   * Set the artifact count (called by the artifacts panel once it lands). M1
   * holds this at 0; exposing it now lets the pill/overlay logic be exercised.
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

  /** Stub artifacts panel: an empty bordered placeholder until the real one lands. */
  private renderArtifactsPanel() {
    const isMobile = this.windowWidth < BREAKPOINT;
    return html`
      <div class="h-full w-full border-l border-border bg-background flex flex-col">
        <div class="flex items-center justify-between px-3 h-10 border-b border-border">
          <span class="text-sm font-medium">${i18n("Artifacts")}</span>
          ${isMobile
            ? html`<button
                class="text-muted-foreground hover:text-foreground text-sm"
                @click=${() => {
                  this.showArtifactsPanel = false;
                  this.requestUpdate();
                }}
              >
                ${i18n("Close")}
              </button>`
            : ""}
        </div>
        <div class="flex-1 flex items-center justify-center text-muted-foreground text-sm">
          ${i18n("No artifacts yet")}
        </div>
      </div>
    `;
  }

  override render() {
    if (!this.agent || !this.agentInterface) {
      return html`<div class="flex items-center justify-center h-full">
        <div class="text-muted-foreground">${i18n("No agent set")}</div>
      </div>`;
    }

    const isMobile = this.windowWidth < BREAKPOINT;
    const showPanel = this.showArtifactsPanel && this.hasArtifacts;

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
          ${showPanel ? this.renderArtifactsPanel() : ""}
        </div>
      </div>
    `;
  }
}
