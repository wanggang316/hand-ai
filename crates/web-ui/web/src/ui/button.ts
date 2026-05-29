// Minimal functional Button helper. A brand-neutral replacement for the shared
// Lit helper the reference frontend imported; only the variants/sizes M1 uses
// are implemented. The full design-system primitive set lands in the theming
// milestone.
//
// M4 adds two small custom elements used by the artifact viewers:
// `<copy-button>` (copies a text payload to the clipboard) and a
// `DownloadButton()` helper (triggers a Blob download of text or binary
// content). Both are intentionally tiny; the full primitive set lands later.

import { html, LitElement, type TemplateResult } from "lit";
import { customElement, property, state } from "lit/decorators.js";
import { Check, Copy, Download } from "lucide";
import { icon } from "./icons";
import { i18n } from "../utils/i18n";

export type ButtonVariant = "default" | "ghost" | "outline" | "destructive";
export type ButtonSize = "default" | "sm" | "icon";

export interface ButtonProps {
  variant?: ButtonVariant;
  size?: ButtonSize;
  className?: string;
  disabled?: boolean;
  title?: string;
  onClick?: (e: MouseEvent) => void;
  children: TemplateResult | string;
}

const VARIANT_CLASS: Record<ButtonVariant, string> = {
  default: "bg-primary text-primary-foreground hover:bg-primary/90",
  ghost: "hover:bg-muted hover:text-foreground",
  outline: "border border-border bg-transparent hover:bg-muted",
  destructive: "bg-destructive text-destructive-foreground hover:bg-destructive/90",
};

const SIZE_CLASS: Record<ButtonSize, string> = {
  default: "h-9 px-4 py-2 text-sm",
  sm: "h-8 px-3 text-xs",
  icon: "h-9 w-9",
};

export function Button(props: ButtonProps): TemplateResult {
  const variant = props.variant ?? "default";
  const size = props.size ?? "default";
  const cls = [
    "inline-flex items-center justify-center rounded-md font-medium",
    "transition-colors disabled:pointer-events-none disabled:opacity-50",
    VARIANT_CLASS[variant],
    SIZE_CLASS[size],
    props.className ?? "",
  ]
    .filter(Boolean)
    .join(" ");

  return html`<button
    class=${cls}
    ?disabled=${props.disabled ?? false}
    title=${props.title ?? ""}
    @click=${(e: MouseEvent) => props.onClick?.(e)}
  >
    ${props.children}
  </button>`;
}

/**
 * `<copy-button .text=${string} .showText=${boolean}>` — copies its `text`
 * property to the clipboard, briefly swapping the icon to a check mark.
 */
@customElement("copy-button")
export class CopyButton extends LitElement {
  @property() text = "";
  @property({ type: Boolean }) showText = true;
  @property() override title = "";
  @state() private copied = false;

  protected override createRenderRoot(): HTMLElement | DocumentFragment {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "inline-flex";
  }

  private async copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(this.text || "");
      this.copied = true;
      setTimeout(() => {
        this.copied = false;
      }, 1500);
    } catch (e) {
      console.error("Copy failed", e);
    }
  }

  override render(): TemplateResult {
    return html`<button
      @click=${() => this.copy()}
      class="inline-flex items-center justify-center gap-1 h-8 w-8 rounded-md hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
      title=${this.title || i18n("Copy")}
    >
      ${this.copied ? icon(Check, "sm") : icon(Copy, "sm")}
      ${this.showText
        ? html`<span class="text-xs">${this.copied ? i18n("Copied!") : i18n("Copy")}</span>`
        : ""}
    </button>`;
  }
}

export interface DownloadButtonProps {
  content: string | Uint8Array;
  filename: string;
  mimeType: string;
  title?: string;
}

/**
 * Trigger a Blob download of text or binary content via a temporary anchor.
 * Returns a Lit template for a small download button.
 */
export function DownloadButton(props: DownloadButtonProps): TemplateResult {
  const download = () => {
    try {
      const part: BlobPart =
        typeof props.content === "string"
          ? props.content
          : (props.content.buffer.slice(
              props.content.byteOffset,
              props.content.byteOffset + props.content.byteLength,
            ) as ArrayBuffer);
      const blob = new Blob([part], { type: props.mimeType });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = props.filename;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (e) {
      console.error("Download failed", e);
    }
  };

  return html`<button
    @click=${download}
    class="inline-flex items-center justify-center h-8 w-8 rounded-md hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
    title=${props.title ?? i18n("Download")}
  >
    ${icon(Download, "sm")}
  </button>`;
}
