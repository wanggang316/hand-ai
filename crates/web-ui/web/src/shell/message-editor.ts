// Bottom-anchored input widget. Auto-growing textarea (max-height 200px), with
// Enter-to-send / Shift+Enter-for-newline and an IME composition guard so a
// composing CJK input never sends. Left toolbar: a paperclip button (opens a
// file picker; spins a loader while files are ingesting) and a thinking-level
// selector shown only when the active model supports reasoning. Right toolbar: a
// model-id button and a send/stop toggle reflecting streaming.
//
// Attachments (M6): files arrive via the paperclip picker, drag-and-drop (with a
// drop overlay), or clipboard paste of images. Each file is ingested through
// `loadAttachment` and shown as an `<attachment-tile>` in a row above the
// textarea, with delete. Limits: max 10 attachments, 20MB each, accepted types.

import { html, LitElement } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { createRef, ref } from "lit/directives/ref.js";
import { Loader2, Paperclip, Send, Square } from "lucide";
import { loadAttachment } from "../attachments/attachment-utils";
import "../attachments/attachment-tile";
import type { Attachment } from "../core/messages";
import type { Model, ThinkingLevel } from "../core/model";
import { Button } from "../ui/button";
import { icon } from "../ui/icons";
import { Select, type SelectOption } from "../ui/select";
import { i18n } from "../utils/i18n";

@customElement("message-editor")
export class MessageEditor extends LitElement {
  private _value = "";
  private textareaRef = createRef<HTMLTextAreaElement>();

  @property()
  get value(): string {
    return this._value;
  }
  set value(val: string) {
    const old = this._value;
    this._value = val;
    this.requestUpdate("value", old);
  }

  @property({ type: Boolean }) isStreaming = false;
  @property({ attribute: false }) currentModel?: Model;
  @property() thinkingLevel: ThinkingLevel = "off";
  @property({ type: Boolean }) showAttachmentButton = true;
  @property({ type: Boolean }) showModelSelector = true;
  @property({ type: Boolean }) showThinkingSelector = true;
  @property({ attribute: false }) onInput?: (value: string) => void;
  @property({ attribute: false }) onSend?: (input: string, attachments: Attachment[]) => void;
  @property({ attribute: false }) onAbort?: () => void;
  @property({ attribute: false }) onModelSelect?: () => void;
  @property({ attribute: false }) onThinkingChange?: (level: ThinkingLevel) => void;
  @property({ attribute: false }) onFilesChange?: (files: Attachment[]) => void;
  @property({ type: Array }) attachments: Attachment[] = [];
  @property({ type: Number }) maxFiles = 10;
  @property({ type: Number }) maxFileSize = 20 * 1024 * 1024; // 20MB
  @property() acceptedTypes =
    "image/*,application/pdf,.docx,.pptx,.xlsx,.xls,.txt,.md,.json,.xml,.html,.css,.js,.ts,.jsx,.tsx,.yml,.yaml";

  @state() processingFiles = false;
  @state() private isDragging = false;
  @state() private errorMessage = "";
  private fileInputRef = createRef<HTMLInputElement>();
  private errorTimer?: ReturnType<typeof setTimeout>;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    if (this.errorTimer) clearTimeout(this.errorTimer);
  }

  /**
   * Show a non-blocking, auto-dismissing validation error inline in the editor
   * (replaces the former `window.alert` sites — alerts block the UI thread and
   * are untestable). The latest message wins; it clears after a few seconds.
   */
  private showError(message: string): void {
    this.errorMessage = message;
    if (this.errorTimer) clearTimeout(this.errorTimer);
    this.errorTimer = setTimeout(() => {
      this.errorMessage = "";
    }, 5000);
  }

  /**
   * Surface a transient, non-blocking notice in the editor's inline slot. Used
   * by the host view to explain why a send was held back (e.g. the connection
   * is still coming up) without clearing the typed text.
   */
  public notify(message: string): void {
    this.showError(message);
  }

  /**
   * Ingest a list of files through `loadAttachment`, enforcing the count and
   * per-file size limits, and append the successes to `attachments`.
   */
  private async ingestFiles(files: File[]): Promise<void> {
    if (files.length === 0) return;

    if (files.length + this.attachments.length > this.maxFiles) {
      this.showError(i18n("Maximum {n} files allowed", { n: this.maxFiles }));
      return;
    }

    this.processingFiles = true;
    const maxMb = Math.round(this.maxFileSize / 1024 / 1024);
    const newAttachments: Attachment[] = [];

    for (const file of files) {
      try {
        if (file.size > this.maxFileSize) {
          this.showError(i18n("{name} exceeds the maximum size of {mb}MB", { name: file.name, mb: maxMb }));
          continue;
        }
        newAttachments.push(await loadAttachment(file));
      } catch (error) {
        console.error(`Error processing ${file.name}:`, error);
        this.showError(i18n("Failed to process {name}: {error}", { name: file.name, error: String(error) }));
      }
    }

    this.attachments = [...this.attachments, ...newAttachments];
    this.onFilesChange?.(this.attachments);
    this.processingFiles = false;
  }

  private removeFile(fileId: string) {
    this.attachments = this.attachments.filter((f) => f.id !== fileId);
    this.onFilesChange?.(this.attachments);
  }

  private handleTextareaInput = (e: Event) => {
    const textarea = e.target as HTMLTextAreaElement;
    this.value = textarea.value;
    this.onInput?.(this.value);
  };

  private handleKeyDown = (e: KeyboardEvent) => {
    // Ignore key events during IME composition (e.g. CJK input).
    if (e.isComposing || e.key === "Process") return;

    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (
        !this.isStreaming &&
        !this.processingFiles &&
        (this.value.trim() || this.attachments.length > 0)
      ) {
        this.handleSend();
      }
    } else if (e.key === "Escape" && this.isStreaming) {
      e.preventDefault();
      this.onAbort?.();
    }
  };

  private handleSend = () => {
    this.onSend?.(this.value, this.attachments);
  };

  private handleAttachmentClick = () => {
    this.fileInputRef.value?.click();
  };

  private handleFilesSelected = async (e: Event) => {
    const input = e.target as HTMLInputElement;
    const files = Array.from(input.files ?? []);
    await this.ingestFiles(files);
    input.value = ""; // Reset so picking the same file again re-fires change.
  };

  private handlePaste = async (e: ClipboardEvent) => {
    const items = e.clipboardData?.items;
    if (!items) return;

    const imageFiles: File[] = [];
    for (let i = 0; i < items.length; i++) {
      const item = items[i];
      if (item.type.startsWith("image/")) {
        const file = item.getAsFile();
        if (file) imageFiles.push(file);
      }
    }

    if (imageFiles.length > 0) {
      e.preventDefault(); // Don't also paste the image as text/markup.
      await this.ingestFiles(imageFiles);
    }
  };

  private handleDragOver = (e: DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (!this.isDragging) this.isDragging = true;
  };

  private handleDragLeave = (e: DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    // Only clear when the pointer actually left the component bounds (drag
    // events fire on child elements too).
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    if (
      e.clientX <= rect.left ||
      e.clientX >= rect.right ||
      e.clientY <= rect.top ||
      e.clientY >= rect.bottom
    ) {
      this.isDragging = false;
    }
  };

  private handleDrop = async (e: DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    this.isDragging = false;
    await this.ingestFiles(Array.from(e.dataTransfer?.files ?? []));
  };

  override firstUpdated() {
    this.textareaRef.value?.focus();
  }

  override render() {
    const supportsThinking = this.currentModel?.reasoning === true;

    return html`
      <div
        class="bg-card rounded-xl shadow-sm relative ${this.isDragging
          ? "border-2 border-primary bg-primary/5"
          : "border border-border"}"
        @dragover=${this.handleDragOver}
        @dragleave=${this.handleDragLeave}
        @drop=${this.handleDrop}
      >
        ${this.isDragging
          ? html`<div
              class="absolute inset-0 bg-primary/10 rounded-xl pointer-events-none z-10 flex items-center justify-center"
            >
              <div class="text-primary font-medium">${i18n("Drop files here")}</div>
            </div>`
          : ""}
        ${this.errorMessage
          ? html`<div
              class="mx-4 mt-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 flex items-start justify-between gap-2"
              role="alert"
            >
              <span class="text-xs text-destructive">${this.errorMessage}</span>
              <button
                class="text-destructive/70 hover:text-destructive text-xs leading-none"
                title=${i18n("Close")}
                @click=${() => {
                  this.errorMessage = "";
                }}
              >
                ✕
              </button>
            </div>`
          : ""}
        ${this.attachments.length > 0
          ? html`<div class="px-4 pt-3 pb-2 flex flex-wrap gap-2">
              ${this.attachments.map(
                (attachment) => html`<attachment-tile
                  .attachment=${attachment}
                  .showDelete=${true}
                  .onDelete=${() => this.removeFile(attachment.id)}
                ></attachment-tile>`,
              )}
            </div>`
          : ""}

        <textarea
          class="w-full bg-transparent p-4 text-foreground placeholder-muted-foreground outline-none resize-none overflow-y-auto"
          placeholder=${i18n("Type a message...")}
          rows="1"
          style="max-height: 200px; field-sizing: content; min-height: 1lh; height: auto;"
          .value=${this.value}
          @input=${this.handleTextareaInput}
          @keydown=${this.handleKeyDown}
          @paste=${this.handlePaste}
          ${ref(this.textareaRef)}
        ></textarea>

        <input
          type="file"
          ${ref(this.fileInputRef)}
          @change=${this.handleFilesSelected}
          accept=${this.acceptedTypes}
          multiple
          style="display: none;"
        />

        <!-- Button row -->
        <div class="px-2 pb-2 flex items-center justify-between">
          <!-- Left: attachment + thinking selector -->
          <div class="flex gap-2 items-center">
            ${this.showAttachmentButton
              ? this.processingFiles
                ? html`<div class="h-8 w-8 flex items-center justify-center">
                    ${icon(Loader2, "sm", "animate-spin text-muted-foreground")}
                  </div>`
                : Button({
                    variant: "ghost",
                    size: "icon",
                    className: "h-8 w-8",
                    onClick: this.handleAttachmentClick,
                    title: i18n("Attach files"),
                    children: icon(Paperclip, "sm"),
                  })
              : ""}
            ${supportsThinking && this.showThinkingSelector
              ? Select({
                  value: this.thinkingLevel,
                  placeholder: i18n("Off"),
                  options: [
                    { value: "off", label: i18n("Off") },
                    { value: "minimal", label: i18n("Minimal") },
                    { value: "low", label: i18n("Low") },
                    { value: "medium", label: i18n("Medium") },
                    { value: "high", label: i18n("High") },
                  ] as SelectOption[],
                  onChange: (value: string) => {
                    const level = value as ThinkingLevel;
                    this.thinkingLevel = level;
                    this.onThinkingChange?.(level);
                  },
                  width: "90px",
                  size: "sm",
                  variant: "ghost",
                  fitContent: true,
                })
              : ""}
          </div>

          <!-- Right: model selector + send/stop toggle -->
          <div class="flex gap-2 items-center">
            ${this.showModelSelector && this.currentModel
              ? Button({
                  variant: "ghost",
                  size: "sm",
                  className: "h-8 text-xs truncate",
                  onClick: () => {
                    this.textareaRef.value?.focus();
                    // Defer past the current click so the dialog's outside-click
                    // close does not catch this same click. setTimeout (not rAF)
                    // so it still fires in a backgrounded/headless tab.
                    setTimeout(() => this.onModelSelect?.(), 0);
                  },
                  children: html`<span class="ml-1">${this.currentModel.id}</span>`,
                })
              : ""}
            ${this.isStreaming
              ? Button({
                  variant: "ghost",
                  size: "icon",
                  className: "h-8 w-8",
                  onClick: () => this.onAbort?.(),
                  title: i18n("Stop"),
                  children: icon(Square, "sm"),
                })
              : Button({
                  variant: "ghost",
                  size: "icon",
                  className: "h-8 w-8",
                  onClick: this.handleSend,
                  disabled:
                    (!this.value.trim() && this.attachments.length === 0) ||
                    this.processingFiles,
                  title: i18n("Send"),
                  children: html`<div style="transform: rotate(-45deg)">${icon(Send, "sm")}</div>`,
                })}
          </div>
        </div>
      </div>
    `;
  }
}
