// `<pdf-artifact>` — renders a base64 PDF artifact, all pages, onto canvases at
// scale 1.5 using pdfjs-dist. The pdfjs worker is configured as a Vite static
// asset (the `?url` import resolves to a hashed asset URL at build time). Header:
// download.

import { html, type TemplateResult } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import * as pdfjsLib from "pdfjs-dist";
// Vite resolves `?url` to the emitted worker asset URL.
import pdfWorkerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { DownloadButton } from "../ui/button";
import { i18n } from "../utils/i18n";
import { ArtifactElement } from "./artifact-element";

pdfjsLib.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;

@customElement("pdf-artifact")
export class PdfArtifact extends ArtifactElement {
  @property({ type: String }) private _content = "";
  @state() private error: string | null = null;
  private currentLoadingTask: { destroy: () => void; promise: Promise<unknown> } | null = null;

  override get content(): string {
    return this._content;
  }
  override set content(value: string) {
    this._content = value;
    this.error = null;
    this.requestUpdate();
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
    this.style.height = "100%";
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.cleanup();
  }

  private cleanup() {
    if (this.currentLoadingTask) {
      this.currentLoadingTask.destroy();
      this.currentLoadingTask = null;
    }
  }

  private decodeBase64ToArrayBuffer(): ArrayBuffer {
    let base64Data = this._content;
    if (this._content.startsWith("data:")) {
      const m = this._content.match(/base64,(.+)/);
      if (m) base64Data = m[1];
    }
    const binaryString = atob(base64Data);
    const bytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) bytes[i] = binaryString.charCodeAt(i);
    return bytes.buffer;
  }

  private decodeBase64(): Uint8Array {
    return new Uint8Array(this.decodeBase64ToArrayBuffer());
  }

  public getHeaderButtons() {
    return html`
      <div class="flex items-center gap-1">
        ${DownloadButton({
          content: this.decodeBase64(),
          filename: this.filename,
          mimeType: "application/pdf",
          title: i18n("Download"),
        })}
      </div>
    `;
  }

  override async updated(changedProperties: Map<string, unknown>) {
    super.updated(changedProperties);
    if (changedProperties.has("_content") && this._content && !this.error) {
      await this.renderPdf();
    }
  }

  private async renderPdf() {
    const container = this.querySelector("#pdf-container");
    if (!container || !this._content) return;

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let pdf: any = null;
    try {
      const arrayBuffer = this.decodeBase64ToArrayBuffer();

      if (this.currentLoadingTask) this.currentLoadingTask.destroy();
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      this.currentLoadingTask = pdfjsLib.getDocument({ data: arrayBuffer }) as any;
      pdf = await this.currentLoadingTask!.promise;
      this.currentLoadingTask = null;

      container.innerHTML = "";
      const wrapper = document.createElement("div");
      wrapper.className = "p-4";
      container.appendChild(wrapper);

      for (let pageNum = 1; pageNum <= pdf.numPages; pageNum++) {
        const page = await pdf.getPage(pageNum);
        const pageContainer = document.createElement("div");
        pageContainer.className = "mb-4 last:mb-0";

        const canvas = document.createElement("canvas");
        const context = canvas.getContext("2d");
        const viewport = page.getViewport({ scale: 1.5 });
        canvas.height = viewport.height;
        canvas.width = viewport.width;
        canvas.className =
          "w-full max-w-full h-auto block mx-auto bg-white rounded shadow-sm border border-border";

        if (context) {
          context.fillStyle = "white";
          context.fillRect(0, 0, canvas.width, canvas.height);
        }

        await page.render({ canvasContext: context!, viewport, canvas }).promise;
        pageContainer.appendChild(canvas);

        if (pageNum < pdf.numPages) {
          const separator = document.createElement("div");
          separator.className = "h-px bg-border my-4";
          pageContainer.appendChild(separator);
        }
        wrapper.appendChild(pageContainer);
      }
    } catch (error) {
      console.error("Error rendering PDF:", error);
      this.error = (error as Error)?.message || i18n("Failed to load PDF");
    } finally {
      if (pdf) pdf.destroy();
    }
  }

  override render(): TemplateResult {
    if (this.error) {
      return html`
        <div class="h-full flex items-center justify-center bg-background p-4">
          <div class="bg-destructive/10 border border-destructive text-destructive p-4 rounded-lg max-w-2xl">
            <div class="font-medium mb-1">${i18n("Error loading PDF")}</div>
            <div class="text-sm opacity-90">${this.error}</div>
          </div>
        </div>
      `;
    }

    return html`
      <div class="h-full flex flex-col bg-background overflow-auto">
        <div id="pdf-container" class="flex-1 overflow-auto"></div>
      </div>
    `;
  }
}

declare global {
  interface HTMLElementTagNameMap {
    "pdf-artifact": PdfArtifact;
  }
}
