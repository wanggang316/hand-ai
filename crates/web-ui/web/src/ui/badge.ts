// Minimal Badge helper. Brand-neutral replacement for the shared helper the
// reference imported; used by the floating artifacts pill in M1.

import { html, type TemplateResult } from "lit";

export function Badge(children: TemplateResult | string): TemplateResult {
  return html`<span
    class="inline-flex items-center rounded-full border border-transparent bg-primary text-primary-foreground px-2.5 py-1 text-xs font-medium shadow-sm"
  >
    ${children}
  </span>`;
}
