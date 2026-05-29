// Default tool renderer. Used as the fallback for any tool without a specific
// renderer, and forced for every tool when show-JSON mode is on. Derives the
// render state, pretty-prints params (handling both a streaming JSON string and
// an already-parsed object), and shows Input / Output code-blocks.

import { html } from "lit";
import { Code } from "lucide";
import type { ToolResultMessage } from "../core/messages";
import { i18n } from "../utils/i18n";
import { renderHeader, type ToolRenderer, type ToolRenderResult } from "./renderer-registry";

export class DefaultRenderer implements ToolRenderer {
  render(params: unknown, result: ToolResultMessage | undefined, isStreaming?: boolean): ToolRenderResult {
    const state = result ? (result.isError ? "error" : "complete") : isStreaming ? "inprogress" : "complete";

    // Format params as pretty JSON. Tool arguments arrive either as a streaming
    // JSON string or as an already-parsed value, so try both.
    let paramsJson = "";
    if (params !== undefined && params !== null && params !== "") {
      try {
        paramsJson = JSON.stringify(JSON.parse(params as string), null, 2);
      } catch {
        try {
          paramsJson = JSON.stringify(params, null, 2);
        } catch {
          paramsJson = String(params);
        }
      }
    }

    // With result: header + params + result.
    if (result) {
      let outputJson =
        result.content
          ?.filter((c) => c.type === "text")
          .map((c) => c.text ?? "")
          .join("\n") || i18n("(no output)");
      let outputLanguage = "text";

      // Pretty-print if the output is valid JSON.
      try {
        outputJson = JSON.stringify(JSON.parse(outputJson), null, 2);
        outputLanguage = "json";
      } catch {
        // Not JSON; leave as-is.
      }

      return {
        content: html`
          <div class="space-y-3">
            ${renderHeader(state, Code, i18n("Tool Call"))}
            ${paramsJson
              ? html`<div>
                  <div class="text-xs font-medium mb-1 text-muted-foreground">${i18n("Input")}</div>
                  <code-block .code=${paramsJson} language="json"></code-block>
                </div>`
              : ""}
            <div>
              <div class="text-xs font-medium mb-1 text-muted-foreground">${i18n("Output")}</div>
              <code-block .code=${outputJson} language=${outputLanguage}></code-block>
            </div>
          </div>
        `,
        isCustom: false,
      };
    }

    // Params only (streaming or waiting for result).
    if (params !== undefined && params !== null && params !== "") {
      if (isStreaming && (!paramsJson || paramsJson === "{}" || paramsJson === "null")) {
        return {
          content: html`<div>${renderHeader(state, Code, i18n("Preparing tool parameters..."))}</div>`,
          isCustom: false,
        };
      }

      return {
        content: html`
          <div class="space-y-3">
            ${renderHeader(state, Code, i18n("Tool Call"))}
            <div>
              <div class="text-xs font-medium mb-1 text-muted-foreground">${i18n("Input")}</div>
              <code-block .code=${paramsJson} language="json"></code-block>
            </div>
          </div>
        `,
        isCustom: false,
      };
    }

    // No params or result yet.
    return {
      content: html`<div>${renderHeader(state, Code, i18n("Preparing tool..."))}</div>`,
      isCustom: false,
    };
  }
}
