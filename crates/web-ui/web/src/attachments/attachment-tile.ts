// `<attachment-tile>` — a compact attachment preview. Renders either an image /
// PDF-first-page thumbnail (when `attachment.preview` is present) or a type icon
// with a truncated file name. A small format badge ("PDF") overlays image-style
// thumbnails for PDFs. In `showDelete` mode a delete button (top-right) fires the
// `onDelete` callback; clicking the tile opens the full-screen `<attachment-overlay>`.
//
// Side-effect import of the overlay registers the custom element and pulls in a
// runtime value (type-only imports would be elided and the element never registered).

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property } from "lit/decorators.js";
import { FileSpreadsheet, FileText, X } from "lucide";
import type { Attachment } from "../core/messages";
import { icon } from "../ui/icons";
import { i18n } from "../utils/i18n";
import { AttachmentOverlay } from "./attachment-overlay";

@customElement("attachment-tile")
export class AttachmentTile extends LitElement {
  @property({ type: Object }) attachment!: Attachment;
  @property({ type: Boolean }) showDelete = false;
  @property({ attribute: false }) onDelete?: () => void;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
    this.classList.add("max-h-16");
  }

  private handleClick = () => {
    AttachmentOverlay.open(this.attachment);
  };

  override render(): TemplateResult {
    const hasPreview = !!this.attachment.preview;
    const isImage = this.attachment.type === "image";
    const isPdf = this.attachment.mimeType === "application/pdf";
    const isExcel =
      this.attachment.mimeType?.includes("spreadsheetml") ||
      this.attachment.fileName.toLowerCase().endsWith(".xlsx") ||
      this.attachment.fileName.toLowerCase().endsWith(".xls");

    const documentIcon = isExcel ? icon(FileSpreadsheet, "md") : icon(FileText, "md");

    return html`
      <div class="relative group inline-block">
        ${hasPreview
          ? html`
              <div class="relative">
                <img
                  src="data:${isImage ? this.attachment.mimeType : "image/png"};base64,${this
                    .attachment.preview}"
                  class="w-16 h-16 object-cover rounded-lg border border-input cursor-pointer hover:opacity-80 transition-opacity"
                  alt=${this.attachment.fileName}
                  title=${this.attachment.fileName}
                  @click=${this.handleClick}
                />
                ${isPdf
                  ? html`
                      <div
                        class="absolute bottom-0 left-0 right-0 bg-background/90 px-1 py-0.5 rounded-b-lg"
                      >
                        <div class="text-[10px] text-muted-foreground text-center font-medium">
                          ${i18n("PDF")}
                        </div>
                      </div>
                    `
                  : ""}
              </div>
            `
          : html`
              <div
                class="w-16 h-16 rounded-lg border border-input cursor-pointer hover:opacity-80 transition-opacity bg-muted text-muted-foreground flex flex-col items-center justify-center p-2"
                @click=${this.handleClick}
                title=${this.attachment.fileName}
              >
                ${documentIcon}
                <div class="text-[10px] text-center truncate w-full">
                  ${this.attachment.fileName.length > 10
                    ? `${this.attachment.fileName.substring(0, 8)}...`
                    : this.attachment.fileName}
                </div>
              </div>
            `}
        ${this.showDelete
          ? html`
              <button
                @click=${(e: Event) => {
                  e.stopPropagation();
                  this.onDelete?.();
                }}
                class="absolute -top-1 -right-1 w-5 h-5 bg-background hover:bg-muted text-muted-foreground hover:text-foreground rounded-full flex items-center justify-center opacity-100 [@media(hover:hover)]:opacity-0 [@media(hover:hover)]:group-hover:opacity-100 transition-opacity border border-input shadow-sm"
                title=${i18n("Remove")}
              >
                ${icon(X, "xs")}
              </button>
            `
          : ""}
      </div>
    `;
  }
}

declare global {
  interface HTMLElementTagNameMap {
    "attachment-tile": AttachmentTile;
  }
}
