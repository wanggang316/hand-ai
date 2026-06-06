// Token/cost formatting helpers. Reproduces the `↑Xk ↓Xk RXk WXk $X.XXXX`
// usage summary the chat shell shows in its per-turn stats bar. The full set of
// model-cost formatters lands with the providers milestone.

import type { Usage } from "../core/messages";
import type { ModelCost } from "../core/model";
import { i18n } from "./i18n";

export function formatCost(cost: number): string {
  return `$${cost.toFixed(4)}`;
}

/**
 * Compact `$in/$out` per-million-token cost summary for the model picker.
 * Returns the localized "Free" label when both rates are zero/absent.
 */
export function formatModelCost(cost: ModelCost | undefined): string {
  if (!cost) return i18n("Free");
  const input = cost.input || 0;
  const output = cost.output || 0;
  if (input === 0 && output === 0) return i18n("Free");

  const formatNum = (num: number): string => {
    if (num >= 100) return num.toFixed(0);
    if (num >= 10) return num.toFixed(1).replace(/\.0$/, "");
    if (num >= 1) return num.toFixed(2).replace(/\.?0+$/, "");
    return num.toFixed(3).replace(/\.?0+$/, "");
  };

  return `$${formatNum(input)}/$${formatNum(output)}`;
}

/**
 * Token-count formatting for the model picker's context/output sizing. Renders
 * as a bare K/M figure (the picker appends the `K` separator itself).
 */
export function formatTokens(tokens: number): string {
  if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(0)}M`;
  if (tokens >= 1000) return `${(tokens / 1000).toFixed(0)}`;
  return String(tokens);
}

export function formatTokenCount(count: number): string {
  if (count < 1000) return count.toString();
  if (count < 10000) return `${(count / 1000).toFixed(1)}k`;
  return `${Math.round(count / 1000)}k`;
}

export function formatUsage(usage: Usage | undefined): string {
  if (!usage) return "";

  const parts: string[] = [];
  if (usage.input) parts.push(`↑${formatTokenCount(usage.input)}`);
  if (usage.output) parts.push(`↓${formatTokenCount(usage.output)}`);
  if (usage.cacheRead) parts.push(`R${formatTokenCount(usage.cacheRead)}`);
  if (usage.cacheWrite) parts.push(`W${formatTokenCount(usage.cacheWrite)}`);
  if (usage.cost?.total) parts.push(formatCost(usage.cost.total));

  return parts.join(" ");
}
