// Bash tool renderer. Three progressive states: no params yet (waiting),
// params-only (streaming the command), and params + result (command + output
// console-block, error variant when the result is an error).

import { html } from "lit";
import { SquareTerminal } from "lucide";
import type { ToolResultMessage } from "../core/messages";
import { i18n } from "../utils/i18n";
import { renderHeader, type ToolRenderer, type ToolRenderResult } from "./renderer-registry";

interface BashParams {
  command?: string;
}

function resultText(result: ToolResultMessage): string {
  return (
    result.content
      ?.filter((c) => c.type === "text")
      .map((c) => c.text ?? "")
      .join("\n") || ""
  );
}

export class BashRenderer implements ToolRenderer<BashParams, unknown> {
  render(params: BashParams | undefined, result: ToolResultMessage | undefined): ToolRenderResult {
    const state = result ? (result.isError ? "error" : "complete") : "inprogress";

    // With result: show command + output.
    if (result && params?.command) {
      const output = resultText(result);
      const combined = output ? `> ${params.command}\n\n${output}` : `> ${params.command}`;
      return {
        content: html`
          <div class="space-y-3">
            ${renderHeader(state, SquareTerminal, i18n("Running command..."))}
            <console-block .content=${combined} .variant=${result.isError ? "error" : "default"}></console-block>
          </div>
        `,
        isCustom: false,
      };
    }

    // Params only (streaming or waiting for result).
    if (params?.command) {
      return {
        content: html`
          <div class="space-y-3">
            ${renderHeader(state, SquareTerminal, i18n("Running command..."))}
            <console-block .content=${`> ${params.command}`}></console-block>
          </div>
        `,
        isCustom: false,
      };
    }

    // No params yet.
    return {
      content: renderHeader(state, SquareTerminal, i18n("Waiting for command...")),
      isCustom: false,
    };
  }
}
