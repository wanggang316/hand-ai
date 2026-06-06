// `<html-artifact>` — runs an HTML artifact live in a sandboxed iframe and
// captures its console output into an `<artifact-console>`. Header actions:
// preview/code toggle, reload, copy HTML, and download a standalone HTML file
// with the runtime injected (so it works offline). The sandbox iframe is loaded
// imperatively via `loadContent`; we inject a `window.complete()` call at the
// end of the document so the runtime signals readiness without timing out.

import { html } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { createRef, type Ref, ref } from "lit/directives/ref.js";
import { RefreshCw } from "lucide";
import { Button, CopyButton, DownloadButton } from "../ui/button";
import { icon } from "../ui/icons";
import "../ui/code-block";
import { i18n } from "../utils/i18n";
import {
  type MessageConsumer,
  RUNTIME_MESSAGE_ROUTER,
  type SandboxIframe,
  type SandboxRuntimeProvider,
} from "../sandbox";
// Side-effect import: registers <sandbox-iframe> (type-only import would be
// elided under isolatedModules and the element would never register).
import "../sandbox/sandboxed-iframe";
import { ArtifactElement } from "./artifact-element";
import "./console";
import type { ArtifactConsole, ArtifactConsoleLog } from "./console";
import "../ui/preview-code-toggle";
import { PreviewCodeToggle } from "../ui/preview-code-toggle";

@customElement("html-artifact")
export class HtmlArtifact extends ArtifactElement {
  @property() override filename = "";
  @property({ attribute: false }) runtimeProviders: SandboxRuntimeProvider[] = [];
  @property({ attribute: false }) sandboxUrlProvider?: () => string;

  private _content = "";
  private logs: ArtifactConsoleLog[] = [];

  public sandboxIframeRef: Ref<SandboxIframe> = createRef();
  private consoleRef: Ref<ArtifactConsole> = createRef();

  @state() private viewMode: "preview" | "code" = "preview";

  private setViewMode(mode: "preview" | "code") {
    this.viewMode = mode;
  }

  public getHeaderButtons() {
    const toggle = new PreviewCodeToggle();
    toggle.mode = this.viewMode;
    toggle.addEventListener("mode-change", (e: Event) => {
      this.setViewMode((e as CustomEvent).detail);
    });

    const copyButton = new CopyButton();
    copyButton.text = this._content;
    copyButton.title = i18n("Copy HTML");
    copyButton.showText = false;

    // Standalone HTML with the runtime injected (no bridge / navigation
    // interceptor) so a downloaded file works offline.
    const sandbox = this.sandboxIframeRef.value;
    const sandboxId = `artifact-${this.filename}`;
    const downloadContent =
      sandbox?.prepareHtmlDocument(sandboxId, this._content, this.runtimeProviders || [], {
        isHtmlArtifact: true,
        isStandalone: true,
      }) || this._content;

    return html`
      <div class="flex items-center gap-2">
        ${toggle}
        ${Button({
          variant: "ghost",
          size: "sm",
          onClick: () => {
            this.logs = [];
            this.executeContent(this._content);
          },
          title: i18n("Reload HTML"),
          children: icon(RefreshCw, "sm"),
        })}
        ${copyButton}
        ${DownloadButton({
          content: downloadContent,
          filename: this.filename,
          mimeType: "text/html",
          title: i18n("Download HTML"),
        })}
      </div>
    `;
  }

  override set content(value: string) {
    const oldValue = this._content;
    this._content = value;
    if (oldValue !== value) {
      this.logs = [];
      this.requestUpdate();
      if (this.sandboxIframeRef.value && value) {
        this.executeContent(value);
      }
    }
  }

  override get content(): string {
    return this._content;
  }

  public executeContent(htmlContent: string) {
    const sandbox = this.sandboxIframeRef.value;
    if (!sandbox) return;

    if (this.sandboxUrlProvider) {
      sandbox.sandboxUrlProvider = this.sandboxUrlProvider;
    }

    const sandboxId = `artifact-${this.filename}`;

    const consumer: MessageConsumer = {
      handleMessage: async (message: unknown): Promise<void> => {
        const m = message as { type?: string; method?: string; text?: string };
        if (m.type === "console") {
          this.logs = [
            ...this.logs,
            { type: m.method === "error" ? "error" : "log", text: m.text ?? "" },
          ];
          this.requestUpdate();
        }
      },
    };

    // HTML artifacts never time out: inject a complete() call at the end of the
    // document so the runtime signals readiness as soon as it loads.
    let modifiedHtml = htmlContent;
    if (modifiedHtml.includes("</html>")) {
      modifiedHtml = modifiedHtml.replace(
        "</html>",
        "<script>if (window.complete) window.complete();</script></html>",
      );
    } else {
      modifiedHtml += "<script>if (window.complete) window.complete();</script>";
    }

    sandbox.loadContent(sandboxId, modifiedHtml, this.runtimeProviders, [consumer]);
  }

  override disconnectedCallback() {
    super.disconnectedCallback();
    const sandboxId = `artifact-${this.filename}`;
    RUNTIME_MESSAGE_ROUTER.unregisterSandbox(sandboxId);
  }

  override firstUpdated() {
    if (this._content && this.sandboxIframeRef.value) {
      this.executeContent(this._content);
    }
  }

  override updated(changedProperties: Map<string | number | symbol, unknown>) {
    super.updated(changedProperties);
    // Execute when the iframe ref becomes available after reconstruction.
    if (this._content && this.sandboxIframeRef.value && this.logs.length === 0) {
      this.executeContent(this._content);
    }
  }

  public getLogs(): string {
    if (this.logs.length === 0) {
      return i18n("No logs for {filename}").replace("{filename}", this.filename);
    }
    return this.logs.map((l) => `[${l.type}] ${l.text}`).join("\n");
  }

  override render() {
    return html`
      <div class="h-full flex flex-col">
        <div class="flex-1 overflow-hidden relative">
          <!-- Preview: always in DOM, hidden when not active -->
          <div
            class="absolute inset-0 flex flex-col"
            style="display: ${this.viewMode === "preview" ? "flex" : "none"}"
          >
            <sandbox-iframe class="flex-1" ${ref(this.sandboxIframeRef)}></sandbox-iframe>
            ${this.logs.length > 0
              ? html`<artifact-console
                  .logs=${this.logs}
                  ${ref(this.consoleRef)}
                ></artifact-console>`
              : ""}
          </div>

          <!-- Code view: always in DOM, hidden when not active -->
          <div
            class="absolute inset-0 overflow-auto bg-background"
            style="display: ${this.viewMode === "code" ? "block" : "none"}"
          >
            <div class="p-4">
              <code-block .code=${this._content} language="html"></code-block>
            </div>
          </div>
        </div>
      </div>
    `;
  }
}

declare global {
  interface HTMLElementTagNameMap {
    "html-artifact": HtmlArtifact;
  }
}
