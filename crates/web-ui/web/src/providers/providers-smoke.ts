// Pure smoke harness for the model-selector fuzzy scorer. The controller can
// call `runFuzzySmoke(query)` without a backend to verify subsequence scoring
// and ranking against a fixed model list (no server, no DOM).

import type { Model } from "../core/model";
import { subsequenceScore } from "./model-selector";

/** A small, fixed catalog covering several providers for ranking checks. */
const SMOKE_MODELS: Model[] = [
  mk("anthropic", "claude-opus-4", "Claude Opus 4"),
  mk("anthropic", "claude-haiku-4-5", "Claude Haiku 4.5"),
  mk("openai", "gpt-4o", "GPT-4o"),
  mk("openai", "gpt-4o-mini", "GPT-4o mini"),
  mk("google", "gemini-2.5-flash", "Gemini 2.5 Flash"),
  mk("openrouter", "z-ai/glm-4.6", "GLM 4.6"),
];

function mk(provider: string, id: string, name: string): Model {
  return {
    id,
    name,
    api: "openai-completions",
    provider,
    reasoning: false,
    input: ["text"],
    contextWindow: 128_000,
    maxTokens: 8192,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  };
}

/**
 * Rank the fixed model list against `query` using the same subsequence scorer
 * the model selector uses, and return the matched model ids in descending
 * score order. Returns `[]` when nothing matches.
 */
export function runFuzzySmoke(query: string): string[] {
  const normalized = query.toLowerCase().replace(/\s+/g, "");
  const scored: { id: string; score: number }[] = [];
  for (const model of SMOKE_MODELS) {
    const text = `${model.provider} ${model.id} ${model.name}`.toLowerCase();
    const score = subsequenceScore(normalized, text);
    if (score > 0) scored.push({ id: model.id, score });
  }
  scored.sort((a, b) => b.score - a.score);
  return scored.map((s) => s.id);
}
