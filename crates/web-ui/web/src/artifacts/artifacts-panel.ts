// `<artifacts-panel>` — the artifacts subsystem's host element.
//
// State is a `Map<string, Artifact>` keyed by filename. Viewer elements are
// inserted imperatively into a single content area (one per artifact, shown /
// hidden by `display`); the tab bar above switches the active file. The panel
// also owns the client `artifacts` AgentTool (create / update / rewrite / get /
// delete / logs) and implements the sandbox `ArtifactsHost` interface so the
// HTML-artifact runtime can round-trip CRUD calls back to the panel.
//
// Server-side routing of the tool call (the `tool_result` RpcCommand and the
// server `artifacts` tool declaration) is a SEPARATE server milestone; this
// panel implements the client `execute` so it is ready once routing lands.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { createRef, type Ref, ref } from "lit/directives/ref.js";
import { X } from "lucide";
import type { Agent } from "../core/agent";
import type { AgentMessage, ToolResultContent } from "../core/messages";
import {
  ARTIFACTS_RUNTIME_PROVIDER_DESCRIPTION_RO,
  ARTIFACTS_TOOL_DESCRIPTION,
  ATTACHMENTS_RUNTIME_DESCRIPTION,
} from "../prompts/prompts";
import {
  ArtifactsRuntimeProvider,
  AttachmentsRuntimeProvider,
  type ArtifactsHost,
  type SandboxRuntimeProvider,
} from "../sandbox";
// Side-effect import: pulling a runtime value from the sandbox registers
// <sandbox-iframe>; importing the artifacts provider value is also load-bearing.
import { Button } from "../ui/button";
import { icon } from "../ui/icons";
import { i18n } from "../utils/i18n";
import { ArtifactElement } from "./artifact-element";
import { DocxArtifact } from "./docx-artifact";
import { ExcelArtifact } from "./excel-artifact";
import { getFileType } from "./file-type";
import { GenericArtifact } from "./generic-artifact";
import { HtmlArtifact } from "./html-artifact";
import { ImageArtifact } from "./image-artifact";
import { MarkdownArtifact } from "./markdown-artifact";
import { PdfArtifact } from "./pdf-artifact";
import { SvgArtifact } from "./svg-artifact";
import { TextArtifact } from "./text-artifact";

/** Simple artifact model. */
export interface Artifact {
  filename: string;
  content: string;
  createdAt: Date;
  updatedAt: Date;
}

/** The artifacts tool parameter shape (LLM-facing). */
export interface ArtifactsParams {
  command: "create" | "update" | "rewrite" | "get" | "delete" | "logs";
  filename: string;
  content?: string;
  old_str?: string;
  new_str?: string;
}

/** JSON-schema description of the artifacts tool parameters. */
const artifactsParamsSchema = {
  type: "object",
  properties: {
    command: {
      type: "string",
      enum: ["create", "update", "rewrite", "get", "delete", "logs"],
      description: "The operation to perform",
    },
    filename: {
      type: "string",
      description: "Filename including extension (e.g., 'index.html', 'script.js')",
    },
    content: { type: "string", description: "File content" },
    old_str: { type: "string", description: "String to replace (for update command)" },
    new_str: { type: "string", description: "Replacement string (for update command)" },
  },
  required: ["command", "filename"],
} as const;

/**
 * The client artifacts AgentTool shape. `execute` matches the reference
 * tool-call contract (toolCallId, args, signal) and returns content blocks +
 * details, so it satisfies both the `ArtifactsHost.tool` interface (used by the
 * HTML-artifact runtime) and the server-routed browser-tool reply path.
 */
export interface ArtifactsAgentTool {
  label: string;
  name: string;
  readonly description: string;
  parameters: unknown;
  execute(
    toolCallId: string,
    args: ArtifactsParams,
    signal?: AbortSignal,
  ): Promise<{ content: ToolResultContent[]; details: undefined }>;
}

@customElement("artifacts-panel")
export class ArtifactsPanel extends LitElement implements ArtifactsHost {
  @state() private _artifacts = new Map<string, Artifact>();
  @state() private _activeFilename: string | null = null;

  private artifactElements = new Map<string, ArtifactElement>();
  private contentRef: Ref<HTMLDivElement> = createRef();

  @property({ attribute: false }) agent?: Agent;
  @property({ attribute: false }) sandboxUrlProvider?: () => string;
  @property({ attribute: false }) onArtifactsChange?: () => void;
  @property({ attribute: false }) onClose?: () => void;
  @property({ attribute: false }) onOpen?: () => void;
  @property({ type: Boolean }) collapsed = false;
  @property({ type: Boolean }) overlay = false;

  /** Public getter for artifacts (also satisfies ArtifactsHost). */
  get artifacts(): Map<string, Artifact> {
    return this._artifacts;
  }

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this; // light DOM for shared styles
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
    this.style.height = "100%";
    // Reattach existing artifact elements when re-inserted into the DOM.
    requestAnimationFrame(() => {
      const container = this.contentRef.value;
      if (!container) return;
      if (!this._activeFilename && this._artifacts.size > 0) {
        this._activeFilename = Array.from(this._artifacts.keys())[0];
      }
      this.artifactElements.forEach((element, name) => {
        if (!element.parentElement) container.appendChild(element);
        element.style.display = name === this._activeFilename ? "block" : "none";
      });
    });
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    // Keep artifact elements to restore on the next mount.
  }

  // --- Runtime providers for HTML artifacts (read-only: attachments + artifacts) ---

  private getHtmlArtifactRuntimeProviders(): SandboxRuntimeProvider[] {
    const providers: SandboxRuntimeProvider[] = [];

    if (this.agent) {
      const attachments = [];
      for (const message of this.agent.state.messages) {
        if (message.role === "user-with-attachments" && message.attachments) {
          attachments.push(...message.attachments);
        }
      }
      if (attachments.length > 0) {
        providers.push(new AttachmentsRuntimeProvider(attachments));
      }
    }

    providers.push(new ArtifactsRuntimeProvider(this, this.agent, false));
    return providers;
  }

  // --- Element lifecycle ---

  private getOrCreateArtifactElement(filename: string, content: string): ArtifactElement {
    let element = this.artifactElements.get(filename);

    if (!element) {
      const type = getFileType(filename);
      if (type === "html") {
        const htmlEl = new HtmlArtifact();
        htmlEl.runtimeProviders = this.getHtmlArtifactRuntimeProviders();
        if (this.sandboxUrlProvider) htmlEl.sandboxUrlProvider = this.sandboxUrlProvider;
        element = htmlEl;
      } else if (type === "svg") {
        element = new SvgArtifact();
      } else if (type === "markdown") {
        element = new MarkdownArtifact();
      } else if (type === "image") {
        element = new ImageArtifact();
      } else if (type === "pdf") {
        element = new PdfArtifact();
      } else if (type === "excel") {
        element = new ExcelArtifact();
      } else if (type === "docx") {
        element = new DocxArtifact();
      } else if (type === "text") {
        element = new TextArtifact();
      } else {
        element = new GenericArtifact();
      }

      element.filename = filename;
      element.content = content;
      element.style.display = "none";
      element.style.height = "100%";

      this.artifactElements.set(filename, element);

      const newElement = element;
      if (this.contentRef.value) {
        this.contentRef.value.appendChild(newElement);
      } else {
        requestAnimationFrame(() => {
          if (this.contentRef.value && !newElement.parentElement) {
            this.contentRef.value.appendChild(newElement);
          }
        });
      }
    } else {
      element.content = content;
      if (element instanceof HtmlArtifact) {
        element.runtimeProviders = this.getHtmlArtifactRuntimeProviders();
      }
    }

    return element;
  }

  private showArtifact(filename: string) {
    requestAnimationFrame(() => {
      this.artifactElements.forEach((element, name) => {
        if (this.contentRef.value && !element.parentElement) {
          this.contentRef.value.appendChild(element);
        }
        element.style.display = name === filename ? "block" : "none";
      });
    });
    this._activeFilename = filename;
    this.requestUpdate(); // tab bar update

    requestAnimationFrame(() => {
      const activeButton = this.querySelector(`button[data-filename="${filename}"]`);
      if (activeButton) {
        activeButton.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "center" });
      }
    });
  }

  /** Open the panel and focus an artifact tab by filename. */
  public openArtifact(filename: string): void {
    if (this._artifacts.has(filename)) {
      this.showArtifact(filename);
      this.onOpen?.();
    }
  }

  // --- The client artifacts AgentTool ---

  public get tool(): ArtifactsAgentTool {
    // eslint-disable-next-line @typescript-eslint/no-this-alias
    const panel = this;
    return {
      label: "Artifacts",
      name: "artifacts",
      get description(): string {
        // HTML artifacts get read-only access to attachments and artifacts.
        const runtimeProviderDescriptions = [
          ATTACHMENTS_RUNTIME_DESCRIPTION,
          ARTIFACTS_RUNTIME_PROVIDER_DESCRIPTION_RO,
        ];
        return ARTIFACTS_TOOL_DESCRIPTION(runtimeProviderDescriptions);
      },
      parameters: artifactsParamsSchema,
      execute: async (_toolCallId: string, args: ArtifactsParams, _signal?: AbortSignal) => {
        const output = await panel.executeCommand(args);
        return { content: [{ type: "text" as const, text: output }], details: undefined };
      },
    };
  }

  // --- Reconstruct artifacts by replaying a message list ---

  public async reconstructFromMessages(messages: AgentMessage[]): Promise<void> {
    const artifactToolName = "artifacts";

    // 1) Collect artifacts-tool calls from assistant messages.
    const toolCalls = new Map<string, { arguments: unknown }>();
    for (const message of messages) {
      if (message.role === "assistant") {
        for (const block of message.content) {
          if (block.type === "toolCall" && block.name === artifactToolName) {
            toolCalls.set(block.id, block);
          }
        }
      }
    }

    // 2) Build an ordered list of successful artifact operations.
    const operations: ArtifactsParams[] = [];
    for (const m of messages) {
      if (m.role === "artifact") {
        switch (m.action) {
          case "create":
            operations.push({ command: "create", filename: m.filename, content: m.content });
            break;
          case "update":
            operations.push({ command: "rewrite", filename: m.filename, content: m.content });
            break;
          case "delete":
            operations.push({ command: "delete", filename: m.filename });
            break;
        }
      } else if (m.role === "toolResult") {
        const tr = m as { toolName?: string; isError?: boolean; toolCallId?: string };
        if (tr.toolName !== artifactToolName || tr.isError) continue;
        const call = tr.toolCallId ? toolCalls.get(tr.toolCallId) : undefined;
        if (!call) continue;
        const params = call.arguments as ArtifactsParams;
        if (params.command === "get" || params.command === "logs") continue;
        operations.push(params);
      }
    }

    // 3) Compute final state per filename by simulating operations in-memory.
    const finalArtifacts = new Map<string, string>();
    for (const op of operations) {
      const filename = op.filename;
      switch (op.command) {
        case "create":
        case "rewrite":
          if (op.content) finalArtifacts.set(filename, op.content);
          break;
        case "update": {
          let existing = finalArtifacts.get(filename);
          if (!existing) break;
          if (op.old_str !== undefined && op.new_str !== undefined) {
            existing = existing.replace(op.old_str, op.new_str);
            finalArtifacts.set(filename, existing);
          }
          break;
        }
        case "delete":
          finalArtifacts.delete(filename);
          break;
      }
    }

    // 4) Reset current UI state before bulk create.
    this._artifacts.clear();
    this.artifactElements.forEach((el) => el.remove());
    this.artifactElements.clear();
    this._activeFilename = null;
    this._artifacts = new Map(this._artifacts);

    // 5) Create artifacts in a single pass without waiting for iframe execution
    //    or tab switching (silent: no per-op onArtifactsChange / show).
    for (const [filename, content] of finalArtifacts.entries()) {
      try {
        await this.createArtifact(
          { command: "create", filename, content },
          { skipWait: true, silent: true },
        );
      } catch {
        // Ignore failures during reconstruction.
      }
    }

    // 6) Show the first artifact if any, then notify listeners exactly once.
    if (!this._activeFilename && this._artifacts.size > 0) {
      this.showArtifact(Array.from(this._artifacts.keys())[0]);
    }
    this.onArtifactsChange?.();
    this.requestUpdate();
  }

  // --- Command dispatch ---

  public async executeCommand(
    params: ArtifactsParams,
    options: { skipWait?: boolean; silent?: boolean } = {},
  ): Promise<string> {
    switch (params.command) {
      case "create":
        return this.createArtifact(params, options);
      case "update":
        return this.updateArtifact(params, options);
      case "rewrite":
        return this.rewriteArtifact(params, options);
      case "get":
        return this.getArtifactContent(params);
      case "delete":
        return this.deleteArtifact(params);
      case "logs":
        return this.getLogs(params);
      default:
        return `Error: Unknown command ${(params as { command: string }).command}`;
    }
  }

  /** Wait up to 1500ms for an HTML artifact's console logs after execution. */
  private async waitForHtmlExecution(filename: string): Promise<string> {
    const element = this.artifactElements.get(filename);
    if (!(element instanceof HtmlArtifact)) return "";
    return new Promise((resolve) => {
      setTimeout(() => resolve(element.getLogs()), 1500);
    });
  }

  /** Re-execute every HTML artifact (they may depend on a changed artifact). */
  private reloadAllHtmlArtifacts(): void {
    this.artifactElements.forEach((element) => {
      if (element instanceof HtmlArtifact && element.sandboxIframeRef.value) {
        element.runtimeProviders = this.getHtmlArtifactRuntimeProviders();
        element.executeContent(element.content);
      }
    });
  }

  private async createArtifact(
    params: ArtifactsParams,
    options: { skipWait?: boolean; silent?: boolean } = {},
  ): Promise<string> {
    if (!params.filename || !params.content) {
      return "Error: create command requires filename and content";
    }
    if (this._artifacts.has(params.filename)) {
      return `Error: File ${params.filename} already exists`;
    }

    const artifact: Artifact = {
      filename: params.filename,
      content: params.content,
      createdAt: new Date(),
      updatedAt: new Date(),
    };
    this._artifacts.set(params.filename, artifact);
    this._artifacts = new Map(this._artifacts);

    this.getOrCreateArtifactElement(params.filename, params.content);
    if (!options.silent) {
      this.showArtifact(params.filename);
      this.onArtifactsChange?.();
      this.requestUpdate();
    }

    this.reloadAllHtmlArtifacts();

    let result = `Created file ${params.filename}`;
    if (getFileType(params.filename) === "html" && !options.skipWait) {
      const logs = await this.waitForHtmlExecution(params.filename);
      result += `\n${logs}`;
    }
    return result;
  }

  private async updateArtifact(
    params: ArtifactsParams,
    options: { skipWait?: boolean; silent?: boolean } = {},
  ): Promise<string> {
    const artifact = this._artifacts.get(params.filename);
    if (!artifact) return this.notFound(params.filename);
    if (!params.old_str || params.new_str === undefined) {
      return "Error: update command requires old_str and new_str";
    }
    if (!artifact.content.includes(params.old_str)) {
      return `Error: String not found in file. Here is the full content:\n\n${artifact.content}`;
    }

    artifact.content = artifact.content.replace(params.old_str, params.new_str);
    artifact.updatedAt = new Date();
    this._artifacts.set(params.filename, artifact);

    this.getOrCreateArtifactElement(params.filename, artifact.content);
    if (!options.silent) {
      this.onArtifactsChange?.();
      this.requestUpdate();
    }
    this.showArtifact(params.filename);
    this.reloadAllHtmlArtifacts();

    let result = `Updated file ${params.filename}`;
    if (getFileType(params.filename) === "html" && !options.skipWait) {
      const logs = await this.waitForHtmlExecution(params.filename);
      result += `\n${logs}`;
    }
    return result;
  }

  private async rewriteArtifact(
    params: ArtifactsParams,
    options: { skipWait?: boolean; silent?: boolean } = {},
  ): Promise<string> {
    const artifact = this._artifacts.get(params.filename);
    if (!artifact) return this.notFound(params.filename);
    if (!params.content) return "Error: rewrite command requires content";

    artifact.content = params.content;
    artifact.updatedAt = new Date();
    this._artifacts.set(params.filename, artifact);

    this.getOrCreateArtifactElement(params.filename, artifact.content);
    if (!options.silent) {
      this.onArtifactsChange?.();
    }
    this.showArtifact(params.filename);
    this.reloadAllHtmlArtifacts();

    let result = "";
    if (getFileType(params.filename) === "html" && !options.skipWait) {
      const logs = await this.waitForHtmlExecution(params.filename);
      result += `\n${logs}`;
    }
    return result;
  }

  private getArtifactContent(params: ArtifactsParams): string {
    const artifact = this._artifacts.get(params.filename);
    if (!artifact) return this.notFound(params.filename);
    return artifact.content;
  }

  private deleteArtifact(params: ArtifactsParams): string {
    const artifact = this._artifacts.get(params.filename);
    if (!artifact) return this.notFound(params.filename);

    this._artifacts.delete(params.filename);
    this._artifacts = new Map(this._artifacts);

    const element = this.artifactElements.get(params.filename);
    if (element) {
      element.remove();
      this.artifactElements.delete(params.filename);
    }

    if (this._activeFilename === params.filename) {
      const remaining = Array.from(this._artifacts.keys());
      if (remaining.length > 0) {
        this.showArtifact(remaining[0]);
      } else {
        this._activeFilename = null;
        this.requestUpdate();
      }
    }
    this.onArtifactsChange?.();
    this.requestUpdate();
    this.reloadAllHtmlArtifacts();

    return `Deleted file ${params.filename}`;
  }

  private getLogs(params: ArtifactsParams): string {
    const element = this.artifactElements.get(params.filename);
    if (!element) return this.notFound(params.filename);
    if (!(element instanceof HtmlArtifact)) {
      return `Error: File ${params.filename} is not an HTML file. Logs are only available for HTML files.`;
    }
    return element.getLogs();
  }

  private notFound(filename: string): string {
    const files = Array.from(this._artifacts.keys());
    if (files.length === 0) {
      return `Error: File ${filename} not found. No files have been created yet.`;
    }
    return `Error: File ${filename} not found. Available files: ${files.join(", ")}`;
  }

  override render(): TemplateResult {
    const artifacts = Array.from(this._artifacts.values());
    const showPanel = artifacts.length > 0 && !this.collapsed;

    return html`
      <div
        class="${showPanel ? "" : "hidden"} ${this.overlay
          ? "fixed inset-0 z-40 pointer-events-auto backdrop-blur-sm bg-background/95"
          : "relative"} h-full flex flex-col bg-background text-card-foreground ${!this.overlay
          ? "border-l border-border"
          : ""} overflow-hidden shadow-xl"
      >
        <!-- Tab bar -->
        <div class="flex items-center justify-between border-b border-border bg-background">
          <div class="flex overflow-x-auto">
            ${artifacts.map((a) => {
              const isActive = a.filename === this._activeFilename;
              const activeClass = isActive
                ? "border-primary text-primary"
                : "border-transparent text-muted-foreground hover:text-foreground";
              return html`
                <button
                  class="px-3 py-2 whitespace-nowrap border-b-2 ${activeClass}"
                  data-filename="${a.filename}"
                  @click=${() => this.showArtifact(a.filename)}
                >
                  <span class="font-mono text-xs">${a.filename}</span>
                </button>
              `;
            })}
          </div>
          <div class="flex items-center gap-1 px-2">
            ${(() => {
              const active = this._activeFilename
                ? this.artifactElements.get(this._activeFilename)
                : undefined;
              return active ? active.getHeaderButtons() : "";
            })()}
            ${Button({
              variant: "ghost",
              size: "sm",
              onClick: () => this.onClose?.(),
              title: i18n("Close artifacts"),
              children: icon(X, "sm"),
            })}
          </div>
        </div>

        <!-- Content area (artifact elements added imperatively) -->
        <div class="flex-1 overflow-hidden" ${ref(this.contentRef)}></div>
      </div>
    `;
  }
}

declare global {
  interface HTMLElementTagNameMap {
    "artifacts-panel": ArtifactsPanel;
  }
}
