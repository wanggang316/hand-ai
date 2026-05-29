// Minimal lucide-backed icon helper. A lucide named export is an `IconNode`
// (an array of `[tag, attrs]` pairs); this renders it into an inline SVG via
// Lit's static-html so it can be dropped into any template. Only the slice of
// the design system M1 needs is built here; the full primitive set lands later.

import { html, svg, type TemplateResult } from "lit";

/**
 * Shape of a lucide named export, e.g. `import { ChevronRight } from "lucide"`.
 * Attribute values may be undefined to stay structurally compatible with
 * lucide's own `IconNode` (its `SVGProps` index signature includes undefined).
 */
export type IconNode = [tag: string, attrs: Record<string, string | number | undefined>][];

export type IconSize = "xs" | "sm" | "md" | "lg" | "xl";

const SIZE_CLASS: Record<IconSize, string> = {
  xs: "w-3 h-3",
  sm: "w-4 h-4",
  md: "w-5 h-5",
  lg: "w-6 h-6",
  xl: "w-8 h-8",
};

function renderChild(
  [tag, attrs]: [string, Record<string, string | number | undefined>],
): TemplateResult {
  switch (tag) {
    case "path":
      return svg`<path d=${String(attrs.d ?? "")}></path>`;
    case "circle":
      return svg`<circle cx=${String(attrs.cx ?? "")} cy=${String(attrs.cy ?? "")} r=${String(attrs.r ?? "")}></circle>`;
    case "rect":
      return svg`<rect x=${String(attrs.x ?? "")} y=${String(attrs.y ?? "")} width=${String(attrs.width ?? "")} height=${String(attrs.height ?? "")} rx=${String(attrs.rx ?? "")} ry=${String(attrs.ry ?? "")}></rect>`;
    case "line":
      return svg`<line x1=${String(attrs.x1 ?? "")} y1=${String(attrs.y1 ?? "")} x2=${String(attrs.x2 ?? "")} y2=${String(attrs.y2 ?? "")}></line>`;
    case "polyline":
      return svg`<polyline points=${String(attrs.points ?? "")}></polyline>`;
    case "polygon":
      return svg`<polygon points=${String(attrs.points ?? "")}></polygon>`;
    case "ellipse":
      return svg`<ellipse cx=${String(attrs.cx ?? "")} cy=${String(attrs.cy ?? "")} rx=${String(attrs.rx ?? "")} ry=${String(attrs.ry ?? "")}></ellipse>`;
    default:
      return svg``;
  }
}

/**
 * Render a lucide icon at the given size with optional extra classes.
 * `icon(ChevronRight, "sm", "text-muted-foreground")`.
 */
export function icon(
  node: IconNode,
  size: IconSize = "sm",
  extraClass = "",
): TemplateResult {
  const cls = `${SIZE_CLASS[size]} ${extraClass}`.trim();
  return html`<svg
    class=${cls}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    ${node.map(renderChild)}
  </svg>`;
}
