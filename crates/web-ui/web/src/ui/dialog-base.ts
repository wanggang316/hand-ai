// DialogBase — a minimal modal base for the de-branded dialog system. Brand-
// neutral reimplementation of the reference mini-lit `Dialog`/`DialogBase`.
//
// Subclasses override `renderContent()` (and optionally `modalWidth` /
// `modalHeight`) and call `open()`. The base appends itself to `document.body`
// on open, renders a fixed backdrop with a centered panel, closes on backdrop
// click or Escape, and removes itself from the DOM on close. Light-DOM
// rendering keeps Tailwind utility classes effective, matching the rest of the
// UI primitives.

import { html, LitElement, type TemplateResult } from "lit";
import { state } from "lit/decorators.js";

export abstract class DialogBase extends LitElement {
  @state() protected isOpen = false;

  /** Panel width; subclasses override for wider dialogs. */
  protected modalWidth = "min(500px, 90vw)";
  /** Panel height; default lets content size the panel up to the viewport. */
  protected modalHeight = "auto";

  private keydownHandler = (e: KeyboardEvent): void => {
    // Ignore key events during IME composition (e.g. CJK input).
    if (e.isComposing || e.key === "Process") return;
    if (e.key === "Escape") {
      e.preventDefault();
      this.close();
    }
  };

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  /** Mount the dialog (if needed) and show it. */
  open(): void {
    if (!this.isConnected) {
      document.body.appendChild(this);
    }
    this.isOpen = true;
    document.addEventListener("keydown", this.keydownHandler);
    this.requestUpdate();
  }

  /** Hide the dialog and remove it from the DOM. */
  close(): void {
    this.isOpen = false;
    document.removeEventListener("keydown", this.keydownHandler);
    this.onClose();
    if (this.isConnected && this.parentElement) {
      this.parentElement.removeChild(this);
    }
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    document.removeEventListener("keydown", this.keydownHandler);
  }

  /** Hook for subclasses to run cleanup/callbacks on close. */
  protected onClose(): void {}

  /** Subclasses render the dialog body here. */
  protected abstract renderContent(): TemplateResult;

  override render(): TemplateResult {
    if (!this.isOpen) return html``;
    return html`
      <div
        class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
        @click=${(e: MouseEvent) => {
          // Close only when the backdrop itself is clicked, not the panel.
          if (e.target === e.currentTarget) this.close();
        }}
      >
        <div
          class="bg-background text-foreground border border-border rounded-lg shadow-xl flex flex-col overflow-hidden max-h-[90vh]"
          style=${`width: ${this.modalWidth}; height: ${this.modalHeight};`}
          @click=${(e: MouseEvent) => e.stopPropagation()}
        >
          ${this.renderContent()}
        </div>
      </div>
    `;
  }
}
