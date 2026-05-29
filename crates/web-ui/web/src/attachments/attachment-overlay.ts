// `<attachment-overlay>` — a full-screen modal viewer for an `Attachment`.
// Opened imperatively via the static `AttachmentOverlay.open(attachment)`, which
// appends an instance to `document.body`. Closes on the backdrop click, the
// Escape key, or the header close button.
//
// Header: file name, an optional "format / Text" toggle (PDF / DOCX / Excel,
// when extracted text exists), a download button, and a close button.
//
// Per-type bodies reuse the same rendering as the M4 artifact viewers:
//   - PDF:   all pages rendered to canvases at scale 1.5 (pdfjs-dist), with the
//            in-flight loading task destroyed on close to free worker resources.
//   - DOCX:  docx-preview `renderAsync` with the artifact style overrides.
//   - Excel: xlsx workbook -> per-sheet HTML tables, with a tab bar for multi-sheet.
//   - PPTX:  the extracted-text `<pre>` (no native slide rendering).
//   - image: the base64 content shown inline.
//   - text:  the decoded text in a `<pre>`.
// Download builds a Blob from the base64 content and triggers a temporary anchor.

import { renderAsync } from "docx-preview";
import { html, LitElement, type TemplateResult } from "lit";
import { state } from "lit/decorators.js";
import { Download, X } from "lucide";
import * as pdfjsLib from "pdfjs-dist";
// Vite resolves `?url` to the emitted worker asset URL (same config as the
// pdf-artifact); the side-effect runtime import keeps the worker bundled.
import pdfWorkerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import * as XLSX from "xlsx";
import type { Attachment } from "../core/messages";
import { Button } from "../ui/button";
import { icon } from "../ui/icons";
import { i18n } from "../utils/i18n";

pdfjsLib.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;

type OverlayFileType = "image" | "pdf" | "docx" | "pptx" | "excel" | "text";

interface PdfLoadingTask {
  destroy: () => void;
  promise: Promise<unknown>;
}

export class AttachmentOverlay extends LitElement {
  @state() private attachment?: Attachment;
  @state() private showExtractedText = false;
  @state() private error: string | null = null;

  private currentLoadingTask: PdfLoadingTask | null = null;
  private onCloseCallback?: () => void;
  private boundHandleKeyDown?: (e: KeyboardEvent) => void;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  /** Append a new overlay to `document.body` showing `attachment`. */
  static open(attachment: Attachment, onClose?: () => void): AttachmentOverlay {
    const overlay = new AttachmentOverlay();
    overlay.attachment = attachment;
    overlay.onCloseCallback = onClose;
    document.body.appendChild(overlay);
    overlay.setupEventListeners();
    return overlay;
  }

  private setupEventListeners() {
    this.boundHandleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") this.close();
    };
    window.addEventListener("keydown", this.boundHandleKeyDown);
  }

  private close() {
    this.cleanup();
    if (this.boundHandleKeyDown) {
      window.removeEventListener("keydown", this.boundHandleKeyDown);
      this.boundHandleKeyDown = undefined;
    }
    this.onCloseCallback?.();
    this.remove();
  }

  private cleanup() {
    this.showExtractedText = false;
    this.error = null;
    if (this.currentLoadingTask) {
      this.currentLoadingTask.destroy();
      this.currentLoadingTask = null;
    }
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    if (this.currentLoadingTask) {
      this.currentLoadingTask.destroy();
      this.currentLoadingTask = null;
    }
    if (this.boundHandleKeyDown) {
      window.removeEventListener("keydown", this.boundHandleKeyDown);
      this.boundHandleKeyDown = undefined;
    }
  }

  private getFileType(): OverlayFileType {
    if (!this.attachment) return "text";
    const { type, mimeType, fileName } = this.attachment;
    const lower = fileName.toLowerCase();
    if (type === "image") return "image";
    if (mimeType === "application/pdf") return "pdf";
    if (mimeType?.includes("wordprocessingml")) return "docx";
    if (mimeType?.includes("presentationml") || lower.endsWith(".pptx")) return "pptx";
    if (
      mimeType?.includes("spreadsheetml") ||
      mimeType?.includes("ms-excel") ||
      lower.endsWith(".xlsx") ||
      lower.endsWith(".xls")
    ) {
      return "excel";
    }
    return "text";
  }

  private getFileTypeLabel(): string {
    switch (this.getFileType()) {
      case "pdf":
        return i18n("PDF");
      case "docx":
        return i18n("Document");
      case "pptx":
        return i18n("Presentation");
      case "excel":
        return i18n("Spreadsheet");
      default:
        return "";
    }
  }

  private handleBackdropClick = () => this.close();

  private handleDownload = () => {
    if (!this.attachment) return;
    try {
      const bytes = this.base64ToUint8Array(this.attachment.content);
      const blob = new Blob([bytes.buffer as ArrayBuffer], { type: this.attachment.mimeType });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = this.attachment.fileName;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (e) {
      console.error("Download failed", e);
    }
  };

  private base64ToUint8Array(base64: string): Uint8Array {
    const binaryString = atob(base64);
    const bytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) bytes[i] = binaryString.charCodeAt(i);
    return bytes;
  }

  private base64ToArrayBuffer(base64: string): ArrayBuffer {
    return this.base64ToUint8Array(base64).buffer as ArrayBuffer;
  }

  override render(): TemplateResult {
    if (!this.attachment) return html``;

    return html`
      <div
        class="fixed inset-0 bg-black/90 z-50 flex flex-col"
        @click=${this.handleBackdropClick}
      >
        <div
          class="bg-background/95 backdrop-blur border-b border-border"
          @click=${(e: Event) => e.stopPropagation()}
        >
          <div class="px-4 py-2 flex items-center justify-between">
            <div class="flex items-center gap-3 min-w-0">
              <span class="text-sm font-medium text-foreground truncate"
                >${this.attachment.fileName}</span
              >
            </div>
            <div class="flex items-center gap-2">
              ${this.renderToggle()}
              ${Button({
                variant: "ghost",
                size: "icon",
                className: "h-8 w-8",
                onClick: this.handleDownload,
                title: i18n("Download"),
                children: icon(Download, "sm"),
              })}
              ${Button({
                variant: "ghost",
                size: "icon",
                className: "h-8 w-8",
                onClick: () => this.close(),
                title: i18n("Close"),
                children: icon(X, "sm"),
              })}
            </div>
          </div>
        </div>

        <div
          class="flex-1 flex items-center justify-center overflow-auto"
          @click=${(e: Event) => e.stopPropagation()}
        >
          ${this.renderContent()}
        </div>
      </div>
    `;
  }

  private renderToggle(): TemplateResult {
    if (!this.attachment) return html``;
    const fileType = this.getFileType();
    const hasExtractedText = !!this.attachment.extractedText;
    const showToggle =
      fileType !== "image" && fileType !== "text" && fileType !== "pptx" && hasExtractedText;
    if (!showToggle) return html``;

    const fileTypeLabel = this.getFileTypeLabel();
    const setText = (value: boolean) => {
      if (this.showExtractedText === value) return;
      this.showExtractedText = value;
      this.error = null;
    };
    const active = "bg-background text-foreground shadow-sm";
    const inactive = "text-muted-foreground hover:text-foreground";

    return html`
      <div class="inline-flex items-center gap-0.5 rounded-md bg-muted p-0.5">
        <button
          @click=${() => setText(false)}
          class="px-2 py-1 text-xs rounded transition-colors ${this.showExtractedText
            ? inactive
            : active}"
          title=${fileTypeLabel}
        >
          ${fileTypeLabel}
        </button>
        <button
          @click=${() => setText(true)}
          class="px-2 py-1 text-xs rounded transition-colors ${this.showExtractedText
            ? active
            : inactive}"
          title=${i18n("Text")}
        >
          ${i18n("Text")}
        </button>
      </div>
    `;
  }

  private renderContent(): TemplateResult {
    if (!this.attachment) return html``;

    if (this.error) {
      return html`
        <div
          class="bg-destructive/10 border border-destructive text-destructive p-4 rounded-lg max-w-2xl"
        >
          <div class="font-medium mb-1">${i18n("Error loading file")}</div>
          <div class="text-sm opacity-90">${this.error}</div>
        </div>
      `;
    }

    return this.renderFileContent();
  }

  private renderFileContent(): TemplateResult {
    if (!this.attachment) return html``;
    const fileType = this.getFileType();

    if (this.showExtractedText && fileType !== "image") {
      return html`
        <div
          class="bg-card border border-border text-foreground p-6 w-full h-full max-w-4xl overflow-auto"
        >
          <pre class="whitespace-pre-wrap font-mono text-xs leading-relaxed">
${this.attachment.extractedText || i18n("No text content available")}</pre
          >
        </div>
      `;
    }

    switch (fileType) {
      case "image": {
        const imageUrl = `data:${this.attachment.mimeType};base64,${this.attachment.content}`;
        return html`
          <img
            src=${imageUrl}
            class="max-w-full max-h-full object-contain rounded-lg shadow-lg"
            alt=${this.attachment.fileName}
          />
        `;
      }
      case "pdf":
        return html`
          <div
            id="attachment-pdf-container"
            class="bg-card text-foreground overflow-auto shadow-lg border border-border w-full h-full max-w-[1000px]"
          ></div>
        `;
      case "docx":
        return html`
          <div
            id="attachment-docx-container"
            class="bg-card text-foreground overflow-auto shadow-lg border border-border w-full h-full max-w-[1000px]"
          ></div>
        `;
      case "excel":
        return html`
          <div
            id="attachment-excel-container"
            class="bg-card text-foreground overflow-auto w-full h-full"
          ></div>
        `;
      case "pptx":
        return html`
          <div
            id="attachment-pptx-container"
            class="bg-card text-foreground overflow-auto shadow-lg border border-border w-full h-full max-w-[1000px]"
          ></div>
        `;
      default:
        return html`
          <div
            class="bg-card border border-border text-foreground p-6 w-full h-full max-w-4xl overflow-auto"
          >
            <pre class="whitespace-pre-wrap font-mono text-sm">
${this.attachment.extractedText || i18n("No content available")}</pre
            >
          </div>
        `;
    }
  }

  override async updated(changedProperties: Map<string, unknown>) {
    super.updated(changedProperties);

    if (
      (changedProperties.has("attachment") || changedProperties.has("showExtractedText")) &&
      this.attachment &&
      !this.showExtractedText &&
      !this.error
    ) {
      switch (this.getFileType()) {
        case "pdf":
          await this.renderPdf();
          break;
        case "docx":
          await this.renderDocx();
          break;
        case "excel":
          await this.renderExcel();
          break;
        case "pptx":
          this.renderExtractedTextBody();
          break;
      }
    }
  }

  private async renderPdf() {
    const container = this.querySelector("#attachment-pdf-container");
    if (!container || !this.attachment) return;

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let pdf: any = null;
    try {
      const arrayBuffer = this.base64ToArrayBuffer(this.attachment.content);

      if (this.currentLoadingTask) this.currentLoadingTask.destroy();
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      this.currentLoadingTask = pdfjsLib.getDocument({ data: arrayBuffer }) as any;
      pdf = await this.currentLoadingTask!.promise;
      this.currentLoadingTask = null;

      container.innerHTML = "";
      const wrapper = document.createElement("div");
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

  private async renderDocx() {
    const container = this.querySelector("#attachment-docx-container");
    if (!container || !this.attachment) return;

    try {
      const arrayBuffer = this.base64ToArrayBuffer(this.attachment.content);
      container.innerHTML = "";

      const wrapper = document.createElement("div");
      wrapper.className = "docx-wrapper-custom";
      container.appendChild(wrapper);

      await renderAsync(arrayBuffer, wrapper as HTMLElement, undefined, {
        className: "docx",
        inWrapper: true,
        ignoreWidth: true,
        ignoreHeight: false,
        ignoreFonts: false,
        breakPages: true,
        ignoreLastRenderedPageBreak: true,
        experimental: false,
        trimXmlDeclaration: true,
        useBase64URL: false,
        renderHeaders: true,
        renderFooters: true,
        renderFootnotes: true,
        renderEndnotes: true,
      });

      const style = document.createElement("style");
      style.textContent = `
        #attachment-docx-container { padding: 0; }
        #attachment-docx-container .docx-wrapper-custom { max-width: 100%; overflow-x: auto; }
        #attachment-docx-container .docx-wrapper {
          max-width: 100% !important;
          margin: 0 !important;
          background: transparent !important;
          padding: 0em !important;
        }
        #attachment-docx-container .docx-wrapper > section.docx {
          box-shadow: none !important;
          border: none !important;
          border-radius: 0 !important;
          margin: 0 !important;
          padding: 2em !important;
          background: white !important;
          color: black !important;
          max-width: 100% !important;
          width: 100% !important;
          min-width: 0 !important;
          overflow-x: auto !important;
        }
        #attachment-docx-container table {
          max-width: 100% !important;
          width: auto !important;
          overflow-x: auto !important;
          display: block !important;
        }
        #attachment-docx-container img { max-width: 100% !important; height: auto !important; }
        #attachment-docx-container p,
        #attachment-docx-container span,
        #attachment-docx-container div {
          max-width: 100% !important;
          word-wrap: break-word !important;
          overflow-wrap: break-word !important;
        }
        #attachment-docx-container .docx-page-break { display: none !important; }
      `;
      container.appendChild(style);
    } catch (error) {
      console.error("Error rendering DOCX:", error);
      this.error = (error as Error)?.message || i18n("Failed to load document");
    }
  }

  private async renderExcel() {
    const container = this.querySelector("#attachment-excel-container");
    if (!container || !this.attachment) return;

    try {
      const arrayBuffer = this.base64ToArrayBuffer(this.attachment.content);
      const workbook = XLSX.read(arrayBuffer, { type: "array" });

      container.innerHTML = "";
      const wrapper = document.createElement("div");
      wrapper.className = "overflow-auto h-full flex flex-col";
      container.appendChild(wrapper);

      if (workbook.SheetNames.length > 1) {
        const tabContainer = document.createElement("div");
        tabContainer.className =
          "flex gap-2 mb-4 border-b border-border sticky top-0 bg-card z-10";

        const sheetContents: HTMLElement[] = [];

        workbook.SheetNames.forEach((sheetName, index) => {
          const tab = document.createElement("button");
          tab.textContent = sheetName;
          tab.className =
            index === 0
              ? "px-4 py-2 text-sm font-medium border-b-2 border-primary text-primary"
              : "px-4 py-2 text-sm font-medium text-muted-foreground hover:text-foreground hover:border-b-2 hover:border-border transition-colors";

          const sheetDiv = document.createElement("div");
          sheetDiv.style.display = index === 0 ? "flex" : "none";
          sheetDiv.className = "flex-1 overflow-auto";
          sheetDiv.appendChild(this.renderExcelSheet(workbook.Sheets[sheetName], sheetName));
          sheetContents.push(sheetDiv);

          tab.onclick = () => {
            tabContainer.querySelectorAll("button").forEach((btn, btnIndex) => {
              btn.className =
                btnIndex === index
                  ? "px-4 py-2 text-sm font-medium border-b-2 border-primary text-primary"
                  : "px-4 py-2 text-sm font-medium text-muted-foreground hover:text-foreground hover:border-b-2 hover:border-border transition-colors";
            });
            sheetContents.forEach((content, contentIndex) => {
              content.style.display = contentIndex === index ? "flex" : "none";
            });
          };

          tabContainer.appendChild(tab);
        });

        wrapper.appendChild(tabContainer);
        sheetContents.forEach((content) => wrapper.appendChild(content));
      } else {
        const sheetName = workbook.SheetNames[0];
        wrapper.appendChild(this.renderExcelSheet(workbook.Sheets[sheetName], sheetName));
      }
    } catch (error) {
      console.error("Error rendering Excel:", error);
      this.error = (error as Error)?.message || i18n("Failed to load spreadsheet");
    }
  }

  private renderExcelSheet(worksheet: XLSX.WorkSheet, sheetName: string): HTMLElement {
    const sheetDiv = document.createElement("div");

    const htmlTable = XLSX.utils.sheet_to_html(worksheet, { id: `sheet-${sheetName}` });
    const tempDiv = document.createElement("div");
    tempDiv.innerHTML = htmlTable;

    const table = tempDiv.querySelector("table");
    if (table) {
      table.className = "w-full border-collapse text-foreground";

      table.querySelectorAll("td, th").forEach((cell) => {
        (cell as HTMLElement).className = "border border-border px-3 py-2 text-sm text-left";
      });

      const headerCells = table.querySelectorAll("thead th, tr:first-child td");
      headerCells.forEach((th) => {
        (th as HTMLElement).className =
          "border border-border px-3 py-2 text-sm font-semibold bg-muted text-foreground sticky top-0";
      });

      table.querySelectorAll("tbody tr:nth-child(even)").forEach((row) => {
        (row as HTMLElement).className = "bg-muted/30";
      });

      sheetDiv.appendChild(table);
    }

    return sheetDiv;
  }

  private renderExtractedTextBody() {
    const container = this.querySelector("#attachment-pptx-container");
    if (!container || !this.attachment) return;

    try {
      container.innerHTML = "";
      const wrapper = document.createElement("div");
      wrapper.className = "p-6 overflow-auto";

      const pre = document.createElement("pre");
      pre.className = "whitespace-pre-wrap text-sm text-foreground font-mono";
      pre.textContent = this.attachment.extractedText || i18n("No text content available");

      wrapper.appendChild(pre);
      container.appendChild(wrapper);
    } catch (error) {
      console.error("Error rendering extracted text:", error);
      this.error = (error as Error)?.message || i18n("Failed to display text content");
    }
  }
}

// Register once (the element is created imperatively, not via a decorator, so the
// guard avoids a double-definition error under HMR / repeated imports).
if (!customElements.get("attachment-overlay")) {
  customElements.define("attachment-overlay", AttachmentOverlay);
}

declare global {
  interface HTMLElementTagNameMap {
    "attachment-overlay": AttachmentOverlay;
  }
}
