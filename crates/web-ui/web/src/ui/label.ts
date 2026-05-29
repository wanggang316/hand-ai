// Minimal functional Label helper. Brand-neutral replacement for the shared
// helper the reference frontend imported.

import { html, type TemplateResult } from "lit";

export interface LabelProps {
  /** Associates the label with a control by id (renders `for=`). */
  for?: string;
  className?: string;
  children: TemplateResult | string;
}

export function Label(props: LabelProps): TemplateResult {
  const cls = ["text-sm font-medium text-foreground", props.className ?? ""].filter(Boolean).join(" ");
  return html`<label class=${cls} for=${props.for ?? ""}>${props.children}</label>`;
}
