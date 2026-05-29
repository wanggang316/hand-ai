// Minimal functional Switch helper (a styled checkbox). Brand-neutral
// replacement for the shared helper the reference frontend imported. Matches
// the accent-primary checkbox style already used by the proxy tab.

import { html, type TemplateResult } from "lit";

export interface SwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  className?: string;
  title?: string;
}

export function Switch(props: SwitchProps): TemplateResult {
  const cls = ["h-4 w-4 cursor-pointer accent-primary disabled:opacity-50", props.className ?? ""]
    .filter(Boolean)
    .join(" ");
  return html`<input
    type="checkbox"
    role="switch"
    class=${cls}
    title=${props.title ?? ""}
    .checked=${props.checked}
    ?disabled=${props.disabled ?? false}
    @change=${(e: Event) => props.onChange((e.target as HTMLInputElement).checked)}
  />`;
}
