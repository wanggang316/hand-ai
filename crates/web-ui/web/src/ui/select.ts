// Minimal functional Select helper backed by a native <select>. Brand-neutral
// replacement for the shared helper the reference imported; M1 only needs the
// thinking-level picker, so this stays a thin styled native control. A richer
// custom select can replace it later without touching call sites.

import { html, type TemplateResult } from "lit";

export interface SelectOption {
  value: string;
  label: string;
  /** Optional leading icon; rendered by callers that build custom menus. */
  icon?: TemplateResult;
}

export interface SelectProps {
  value: string;
  options: SelectOption[];
  placeholder?: string;
  onChange: (value: string) => void;
  width?: string;
  size?: "sm" | "default";
  variant?: "default" | "ghost";
  fitContent?: boolean;
  title?: string;
}

export function Select(props: SelectProps): TemplateResult {
  const sizeClass = props.size === "sm" ? "h-8 text-xs" : "h-9 text-sm";
  const variantClass =
    props.variant === "ghost"
      ? "bg-transparent hover:bg-muted"
      : "border border-border bg-background";
  const style = props.width ? `width: ${props.width};` : "";

  return html`<select
    class="rounded-md px-2 ${sizeClass} ${variantClass} text-foreground outline-none cursor-pointer"
    style=${style}
    title=${props.title ?? ""}
    .value=${props.value}
    @change=${(e: Event) => props.onChange((e.target as HTMLSelectElement).value)}
  >
    ${props.options.map(
      (opt) => html`<option value=${opt.value} ?selected=${opt.value === props.value}>${opt.label}</option>`,
    )}
  </select>`;
}
