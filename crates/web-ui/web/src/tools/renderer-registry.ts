// Tool renderer registry + the unified `renderTool` entry point. A tool renderer
// turns a tool call's params and result into a Lit template; `renderTool` looks
// one up by tool name and falls back to the DefaultRenderer (or forces it when
// show-JSON mode is on). The `renderHeader` / `renderCollapsibleHeader` helpers
// produce the status header rows the renderers share. Renderers themselves are
// data-only (no custom elements) so they can be registered as side effects.

import { html, type TemplateResult } from "lit";
import type { Ref } from "lit/directives/ref.js";
import { ref } from "lit/directives/ref.js";
import { ChevronsUpDown, ChevronUp, Loader } from "lucide";
import type { ToolResultMessage } from "../core/messages";
import type { IconNode } from "../ui/icons";
import { icon } from "../ui/icons";

export type ToolRenderState = "inprogress" | "complete" | "error";

export interface ToolRenderResult {
  content: TemplateResult;
  /** true = no card wrapper (renderer owns its chrome); false = wrap in a card. */
  isCustom: boolean;
}

export interface ToolRenderer<TParams = unknown, TDetails = unknown> {
  render(
    params: TParams | undefined,
    result: ToolResultMessage<TDetails> | undefined,
    isStreaming?: boolean,
  ): ToolRenderResult;
}

// Registry of tool renderers keyed by tool name.
export const toolRenderers = new Map<string, ToolRenderer>();

/** Register a custom tool renderer. */
export function registerToolRenderer(toolName: string, renderer: ToolRenderer): void {
  toolRenderers.set(toolName, renderer);
}

/** Get a tool renderer by name. */
export function getToolRenderer(toolName: string): ToolRenderer | undefined {
  return toolRenderers.get(toolName);
}

// The default renderer is injected by `index.ts` after construction to avoid a
// circular import (DefaultRenderer imports the header helpers from here).
let defaultRenderer: ToolRenderer | undefined;

/** Wire the fallback renderer used when no specific renderer is registered. */
export function setDefaultToolRenderer(renderer: ToolRenderer): void {
  defaultRenderer = renderer;
}

// Global flag: when on, every tool is rendered through the default JSON renderer.
let showJsonMode = false;

/**
 * Enable or disable show-JSON mode. When enabled, all tools render through the
 * default JSON renderer regardless of any registered specific renderer.
 */
export function setShowJsonMode(enabled: boolean): void {
  showJsonMode = enabled;
}

/**
 * Render a tool call. Unified entry point that handles params, result, and the
 * streaming flag, dispatching to the registered renderer or the default.
 */
export function renderTool(
  toolName: string,
  params: unknown,
  result: ToolResultMessage | undefined,
  isStreaming?: boolean,
): ToolRenderResult {
  if (showJsonMode) {
    if (defaultRenderer) return defaultRenderer.render(params, result, isStreaming);
  } else {
    const renderer = getToolRenderer(toolName);
    if (renderer) return renderer.render(params, result, isStreaming);
    if (defaultRenderer) return defaultRenderer.render(params, result, isStreaming);
  }
  // Should not happen once index.ts has wired the default renderer.
  return { content: html`<div class="text-xs text-muted-foreground">${toolName}</div>`, isCustom: false };
}

function statusIcon(iconNode: IconNode, color: string): TemplateResult {
  return html`<span class="inline-block ${color}">${icon(iconNode, "sm")}</span>`;
}

/**
 * Render a status header row. The tool icon shows on the left (colored by
 * state); the spinner shows on the right while in progress.
 */
export function renderHeader(
  state: ToolRenderState,
  toolIcon: IconNode,
  text: string | TemplateResult,
): TemplateResult {
  switch (state) {
    case "inprogress":
      return html`
        <div class="flex items-center justify-between gap-2 text-sm text-muted-foreground">
          <div class="flex items-center gap-2">${statusIcon(toolIcon, "text-foreground")} ${text}</div>
          ${statusIcon(Loader, "text-foreground animate-spin")}
        </div>
      `;
    case "complete":
      return html`
        <div class="flex items-center gap-2 text-sm text-muted-foreground">
          ${statusIcon(toolIcon, "text-green-600 dark:text-green-500")} ${text}
        </div>
      `;
    case "error":
      return html`
        <div class="flex items-center gap-2 text-sm text-muted-foreground">
          ${statusIcon(toolIcon, "text-destructive")} ${text}
        </div>
      `;
  }
}

/**
 * Render a collapsible status header. Same status semantics as `renderHeader`,
 * plus a chevron button that toggles the visibility of a referenced content
 * element via a max-height transition and swaps the chevron icon (refs are used
 * so the toggle mutates the live DOM without a Lit re-render).
 */
export function renderCollapsibleHeader(
  state: ToolRenderState,
  toolIcon: IconNode,
  text: string | TemplateResult,
  contentRef: Ref<HTMLElement>,
  chevronRef: Ref<HTMLElement>,
  defaultExpanded = false,
): TemplateResult {
  const toggleContent = (e: Event) => {
    e.preventDefault();
    const content = contentRef.value;
    const chevron = chevronRef.value;
    if (content && chevron) {
      const isCollapsed = content.classList.contains("max-h-0");
      const upIcon = chevron.querySelector(".chevron-up");
      const downIcon = chevron.querySelector(".chevrons-up-down");
      if (isCollapsed) {
        content.classList.remove("max-h-0");
        content.classList.add("max-h-[2000px]", "mt-3");
        upIcon?.classList.remove("hidden");
        downIcon?.classList.add("hidden");
      } else {
        content.classList.remove("max-h-[2000px]", "mt-3");
        content.classList.add("max-h-0");
        upIcon?.classList.add("hidden");
        downIcon?.classList.remove("hidden");
      }
    }
  };

  const toolIconColor =
    state === "complete"
      ? "text-green-600 dark:text-green-500"
      : state === "error"
        ? "text-destructive"
        : "text-foreground";

  return html`
    <button
      @click=${toggleContent}
      class="flex items-center justify-between gap-2 text-sm text-muted-foreground w-full text-left hover:text-foreground transition-colors cursor-pointer"
    >
      <div class="flex items-center gap-2">
        ${state === "inprogress" ? statusIcon(Loader, "text-foreground animate-spin") : ""}
        ${statusIcon(toolIcon, toolIconColor)} ${text}
      </div>
      <span class="inline-block text-muted-foreground" ${ref(chevronRef)}>
        <span class="chevron-up ${defaultExpanded ? "" : "hidden"}">${icon(ChevronUp, "sm")}</span>
        <span class="chevrons-up-down ${defaultExpanded ? "hidden" : ""}">${icon(ChevronsUpDown, "sm")}</span>
      </span>
    </button>
  `;
}
