// Minimal functional Input helper (`fc()` factory + `Input`). Brand-neutral
// replacement for the shared helper the reference frontend imported. Styled to
// match the inline `<input>`s used by the proxy/provider dialogs so call sites
// can converge on a single primitive.

import { html, type TemplateResult } from "lit";

export type InputType = "text" | "password" | "email" | "url" | "number" | "search";

export interface InputProps {
  value: string;
  onInput: (value: string) => void;
  type?: InputType;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
  /** Fired on the native `change` event (e.g. to persist on blur/commit). */
  onChange?: (value: string) => void;
}

const BASE_CLASS =
  "rounded-md border border-border bg-background px-2 h-9 text-sm text-foreground " +
  "outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring disabled:opacity-50";

/** Functional-component factory: `fc(props)` returns the input template. */
export function fc(props: InputProps): TemplateResult {
  const cls = [BASE_CLASS, props.className ?? ""].filter(Boolean).join(" ");
  return html`<input
    type=${props.type ?? "text"}
    class=${cls}
    .value=${props.value}
    placeholder=${props.placeholder ?? ""}
    ?disabled=${props.disabled ?? false}
    @input=${(e: Event) => props.onInput((e.target as HTMLInputElement).value)}
    @change=${(e: Event) => props.onChange?.((e.target as HTMLInputElement).value)}
  />`;
}

/** Alias kept for call-site readability; identical to `fc`. */
export const Input = fc;
