// Calculate tool renderer. Four progressive text states: no params (waiting),
// empty expression (writing), full expression without result (calculating), and
// full result (`expression = result` in the header, or an error layout below the
// header when the result is an error).

import { html } from "lit";
import { Calculator } from "lucide";
import type { ToolResultMessage } from "../core/messages";
import { i18n } from "../utils/i18n";
import { renderHeader, type ToolRenderer, type ToolRenderResult } from "./renderer-registry";

interface CalculateParams {
  expression?: string;
}

function resultText(result: ToolResultMessage): string {
  return (
    result.content
      ?.filter((c) => c.type === "text")
      .map((c) => c.text ?? "")
      .join("\n") || ""
  );
}

export class CalculateRenderer implements ToolRenderer<CalculateParams, unknown> {
  render(params: CalculateParams | undefined, result: ToolResultMessage | undefined): ToolRenderResult {
    const state = result ? (result.isError ? "error" : "complete") : "inprogress";

    // Full params + full result.
    if (result && params?.expression) {
      const output = resultText(result);

      // Error: expression in header, error message below.
      if (result.isError) {
        return {
          content: html`
            <div class="space-y-3">
              ${renderHeader(state, Calculator, params.expression)}
              <div class="text-sm text-destructive">${output}</div>
            </div>
          `,
          isCustom: false,
        };
      }

      // Success: `expression = result` in header.
      return {
        content: renderHeader(state, Calculator, `${params.expression} = ${output}`),
        isCustom: false,
      };
    }

    // Full params, no result.
    if (params?.expression) {
      return {
        content: renderHeader(state, Calculator, `${i18n("Calculating")} ${params.expression}`),
        isCustom: false,
      };
    }

    // Partial params (empty expression), no result.
    if (params && !params.expression) {
      return { content: renderHeader(state, Calculator, i18n("Writing expression...")), isCustom: false };
    }

    // No params, no result.
    return { content: renderHeader(state, Calculator, i18n("Waiting for expression...")), isCustom: false };
  }
}
