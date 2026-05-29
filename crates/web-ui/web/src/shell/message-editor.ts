// Bottom-anchored input widget. Auto-growing textarea (max-height 200px), with
// Enter-to-send / Shift+Enter-for-newline and an IME composition guard so a
// composing CJK input never sends. Left toolbar: a paperclip button (attachment
// wiring lands in the attachments milestone — rendered here, no-op for now) and
// a thinking-level selector shown only when the active model supports reasoning.
// Right toolbar: a model-id button and a send/stop toggle reflecting streaming.

import { html, LitElement } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { createRef, ref } from "lit/directives/ref.js";
import { Paperclip, Send, Square } from "lucide";
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

  @state() processingFiles = false;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
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
    // Attachment ingestion is wired in the attachments milestone; no-op for now.
  };

  override firstUpdated() {
    this.textareaRef.value?.focus();
  }

  override render() {
    const supportsThinking = this.currentModel?.reasoning === true;

    return html`
      <div class="bg-card rounded-xl border border-border shadow-sm relative">
        <textarea
          class="w-full bg-transparent p-4 text-foreground placeholder-muted-foreground outline-none resize-none overflow-y-auto"
          placeholder=${i18n("Type a message...")}
          rows="1"
          style="max-height: 200px; field-sizing: content; min-height: 1lh; height: auto;"
          .value=${this.value}
          @input=${this.handleTextareaInput}
          @keydown=${this.handleKeyDown}
          ${ref(this.textareaRef)}
        ></textarea>

        <!-- Button row -->
        <div class="px-2 pb-2 flex items-center justify-between">
          <!-- Left: attachment + thinking selector -->
          <div class="flex gap-2 items-center">
            ${this.showAttachmentButton
              ? Button({
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
                    requestAnimationFrame(() => this.onModelSelect?.());
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
