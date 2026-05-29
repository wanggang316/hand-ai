// User-message renderer. Renders the user's text via markdown-block and, for the
// user-with-attachments role, a row of attachment chips. The rich attachment
// tile element lands with the attachments milestone; until then a minimal chip
// (filename) keeps the shape correct without referencing a not-yet-defined tag.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property } from "lit/decorators.js";
import type {
  TextContent,
  UserMessage as UserMessageType,
  UserMessageWithAttachments,
} from "../../core/messages";
import "../../ui/markdown-block";

@customElement("user-message")
export class UserMessage extends LitElement {
  @property({ type: Object }) message!: UserMessageType | UserMessageWithAttachments;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "block";
  }

  override render() {
    let content = "";
    if (typeof this.message.content === "string") {
      content = this.message.content;
    } else {
      const blocks = this.message.content as readonly { type: string }[];
      const textBlock = blocks.find((c): c is TextContent => c.type === "text");
      content = textBlock?.text ?? "";
    }

    const attachments =
      this.message.role === "user-with-attachments" ? this.message.attachments : undefined;

    return html`
      <div class="flex justify-start mx-4">
        <div class="user-message-container py-2 px-4 rounded-xl">
          <markdown-block .content=${content}></markdown-block>
          ${attachments && attachments.length > 0
            ? html`<div class="mt-3 flex flex-wrap gap-2">
                ${attachments.map(
                  (a): TemplateResult => html`<span
                    class="inline-flex items-center gap-1 rounded-md border border-border bg-muted px-2 py-1 text-xs text-muted-foreground"
                    >${a.fileName}</span
                  >`,
                )}
              </div>`
            : ""}
        </div>
      </div>
    `;
  }
}
