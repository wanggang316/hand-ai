// `<markdown-artifact>` — renders a Markdown artifact as a rendered
// `<markdown-block>` preview or as raw code, with a preview/code toggle, copy,
// and download in the header.

import { html } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { CopyButton, DownloadButton } from "../ui/button";
import "../ui/code-block";
import "../ui/markdown-block";
import "../ui/preview-code-toggle";
import { PreviewCodeToggle } from "../ui/preview-code-toggle";
import { i18n } from "../utils/i18n";
import { ArtifactElement } from "./artifact-element";

@customElement("markdown-artifact")
export class MarkdownArtifact extends ArtifactElement {
  @property() override filename = "";

  private _content = "";
  @state() private viewMode: "preview" | "code" = "preview";

  override get content(): string {
    return this._content;
  }
  override set content(value: string) {
    this._content = value;
    this.requestUpdate();
  }

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
    copyButton.title = i18n("Copy Markdown");
    copyButton.showText = false;

    return html`
      <div class="flex items-center gap-2">
        ${toggle}
        ${copyButton}
        ${DownloadButton({
          content: this._content,
          filename: this.filename,
          mimeType: "text/markdown",
          title: i18n("Download Markdown"),
        })}
      </div>
    `;
  }

  override render() {
    return html`
      <div class="h-full flex flex-col">
        <div class="flex-1 overflow-auto">
          ${this.viewMode === "preview"
            ? html`<div class="p-4">
                <markdown-block .content=${this._content}></markdown-block>
              </div>`
            : html`<div class="p-4">
                <code-block .code=${this._content} language="markdown"></code-block>
              </div>`}
        </div>
      </div>
    `;
  }
}

declare global {
  interface HTMLElementTagNameMap {
    "markdown-artifact": MarkdownArtifact;
  }
}
