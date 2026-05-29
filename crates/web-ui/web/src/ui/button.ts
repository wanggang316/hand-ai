// Minimal functional Button helper. A brand-neutral replacement for the shared
// Lit helper the reference frontend imported; only the variants/sizes M1 uses
// are implemented. The full design-system primitive set lands in the theming
// milestone.

import { html, type TemplateResult } from "lit";

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
