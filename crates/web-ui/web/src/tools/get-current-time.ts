// GetCurrentTime tool renderer. Covers the param/result/timezone paths: full
// params + result (success time in header, or error below), result-only, full
// params without result, partial/empty params, and nothing yet.

import { html } from "lit";
import { Clock } from "lucide";
import type { ToolResultMessage } from "../core/messages";
import { i18n } from "../utils/i18n";
import { renderHeader, type ToolRenderer, type ToolRenderResult } from "./renderer-registry";

interface GetCurrentTimeParams {
  timezone?: string;
}

function resultText(result: ToolResultMessage): string {
  return (
    result.content
      ?.filter((c) => c.type === "text")
      .map((c) => c.text ?? "")
      .join("\n") || ""
  );
}

export class GetCurrentTimeRenderer implements ToolRenderer<GetCurrentTimeParams, unknown> {
  render(params: GetCurrentTimeParams | undefined, result: ToolResultMessage | undefined): ToolRenderResult {
    const state = result ? (result.isError ? "error" : "complete") : "inprogress";

    // Full params + full result.
    if (result && params) {
      const output = resultText(result);
      const headerText = params.timezone
        ? `${i18n("Getting current time in")} ${params.timezone}`
        : i18n("Getting current date and time");

      if (result.isError) {
        return {
          content: html`
            <div class="space-y-3">
              ${renderHeader(state, Clock, headerText)}
              <div class="text-sm text-destructive">${output}</div>
            </div>
          `,
          isCustom: false,
        };
      }

      return { content: renderHeader(state, Clock, `${headerText}: ${output}`), isCustom: false };
    }

    // Full result, no params.
    if (result) {
      const output = resultText(result);

      if (result.isError) {
        return {
          content: html`
            <div class="space-y-3">
              ${renderHeader(state, Clock, i18n("Getting current date and time"))}
              <div class="text-sm text-destructive">${output}</div>
            </div>
          `,
          isCustom: false,
        };
      }

      return {
        content: renderHeader(state, Clock, `${i18n("Getting current date and time")}: ${output}`),
        isCustom: false,
      };
    }

    // Full params, no result.
    if (params?.timezone) {
      return {
        content: renderHeader(state, Clock, `${i18n("Getting current time in")} ${params.timezone}`),
        isCustom: false,
      };
    }

    // Partial params (no timezone) or empty params, no result.
    if (params) {
      return { content: renderHeader(state, Clock, i18n("Getting current date and time")), isCustom: false };
    }

    // No params, no result.
    return { content: renderHeader(state, Clock, i18n("Getting time...")), isCustom: false };
  }
}
